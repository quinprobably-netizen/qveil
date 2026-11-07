//! A loopback-only HTTP server: the desktop app's API, a thumbnail fetcher
//! and a video relay.
//!
//! The app and mpv only ever connect to 127.0.0.1. Every request that leaves
//! the machine is made here, through the configured route, so a proxy or Tor
//! covers the video bytes and thumbnails too. Every path starts with a
//! random token, so other local programs and web pages can't use it.
//! Relayed streams forward Range requests, so seeking works over Tor as well.

use crate::app::{self, Runtime, Watched};
use crate::config::Config;
use crate::errors::friendly;
use crate::ids::{self, VideoRef};
use crate::net::{self, TOR_PROXY};
use crate::store::Paths;
use anyhow::{Context, Result, bail};
use reqwest::Method;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant};

const MAX_BODY: usize = 8 << 20;
const MAX_SESSIONS: usize = 4;
const MAX_THUMBS: usize = 800;

// Limits that apply before a client has proven it holds the token, so that a
// local program (or a web page that guessed the port) can't tie up threads or
// memory just by connecting.
const MAX_REQUEST_LINE: u64 = 8 << 10;
const MAX_HEADERS: u64 = 32 << 10;
const MAX_HEADER_COUNT: usize = 100;
const MAX_CONNECTIONS: usize = 64;
/// A client has this long, in total, to send its whole request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Ordinary responses are small, so a client that doesn't read them is stuck.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// A relayed body is different: mpv stops reading while a paused video's
/// cache is full, so a short write timeout would kill a paused video.
const RELAY_WRITE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How long, and how much, to read from a refused client before closing (see `refuse`).
const LINGER: Duration = Duration::from_secs(1);
const LINGER_BYTES: u64 = 64 << 10;
/// Pause after a failed `accept`, so running out of file descriptors doesn't spin a core.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

pub struct Server {
    runtime: RwLock<Arc<Runtime>>,
    pub paths: Paths,
    token: String,
    base: OnceLock<String>,
    sessions: Mutex<VecDeque<(String, Arc<Session>)>>,
    thumbs: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    /// Serializes read-modify-write of the subscription, feed and history files.
    store: Mutex<()>,
    /// Connections being served right now (see `MAX_CONNECTIONS`).
    connections: Arc<AtomicUsize>,
}

struct Session {
    video: String,
    audio: Option<String>,
    headers: Vec<(String, String)>,
    client: Client,
}

#[derive(Serialize)]
pub struct PlayInfo {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(rename = "channelId", skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    /// Absolute loopback URLs.
    pub video: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct ConfigPatch {
    /// "direct", "tor" or "proxy" (with `proxy`).
    route: Option<String>,
    proxy: Option<String>,
    history: Option<bool>,
    thumbnails: Option<bool>,
    max_height: Option<u32>,
}

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn param(&self, name: &str) -> Result<&str> {
        self.query
            .get(name)
            .map(String::as_str)
            .with_context(|| format!("missing {name}"))
    }
    fn num(&self, name: &str, default: usize) -> usize {
        self.query
            .get(name)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
    fn flag(&self, name: &str) -> bool {
        self.query
            .get(name)
            .is_some_and(|v| v == "1" || v == "true")
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn random_hex() -> String {
    format!(
        "{:016x}{:016x}",
        rand::random::<u64>(),
        rand::random::<u64>()
    )
}

impl Server {
    pub fn new(rt: Runtime, paths: Paths) -> Arc<Self> {
        Arc::new(Self {
            runtime: RwLock::new(Arc::new(rt)),
            paths,
            token: random_hex(),
            base: OnceLock::new(),
            sessions: Mutex::new(VecDeque::new()),
            thumbs: Mutex::new(HashMap::new()),
            store: Mutex::new(()),
            connections: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn runtime(&self) -> Arc<Runtime> {
        self.runtime
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Starts listening (once) and returns the base URL, token included.
    pub fn listen(self: &Arc<Self>) -> Result<String> {
        if let Some(b) = self.base.get() {
            return Ok(b.clone());
        }
        let listener = TcpListener::bind("127.0.0.1:0").context("binding a loopback port")?;
        let base = format!(
            "http://127.0.0.1:{}/{}",
            listener.local_addr()?.port(),
            self.token
        );
        let server = Arc::clone(self);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(conn) => server.accept(conn),
                    Err(_) => std::thread::sleep(ACCEPT_BACKOFF),
                }
            }
        });
        Ok(self.base.get_or_init(|| base).clone())
    }

    /// Resolves a video through one pinned identity of the route and registers
    /// relay URLs for it.
    pub fn open(self: &Arc<Self>, v: &VideoRef) -> Result<PlayInfo> {
        let base = self.listen()?;
        let rt = self.runtime();
        let yt = rt.pinned_ytdlp();
        let s = yt.resolve(&v.watch_url(), &rt.cfg.format())?;
        let sid = random_hex();
        let session = Session {
            video: s.video,
            audio: s.audio,
            headers: s.headers,
            client: yt.net.relay_client()?,
        };
        let has_audio = session.audio.is_some();
        {
            let mut sessions = lock(&self.sessions);
            sessions.push_back((sid.clone(), Arc::new(session)));
            while sessions.len() > MAX_SESSIONS {
                sessions.pop_front();
            }
        }
        if rt.cfg.history {
            let _g = lock(&self.store);
            app::record_history(
                &self.paths,
                Watched {
                    id: v.id.clone(),
                    title: s.title.clone(),
                    channel: s.channel.clone(),
                    at: app::now(),
                },
            )?;
        }
        Ok(PlayInfo {
            id: v.id.clone(),
            title: s.title,
            channel: s.channel,
            channel_id: s.channel_id,
            duration: s.duration,
            start: v.start,
            video: format!("{base}/s/{sid}/v"),
            audio: has_audio.then(|| format!("{base}/s/{sid}/a")),
        })
    }

    fn session(&self, sid: &str) -> Option<Arc<Session>> {
        lock(&self.sessions)
            .iter()
            .find(|(id, _)| id == sid)
            .map(|(_, s)| Arc::clone(s))
    }

    /// Serves one connection on its own thread, unless too many are open.
    fn accept(self: &Arc<Self>, mut conn: TcpStream) {
        let Some(slot) = Slot::take(&self.connections) else {
            let _ = conn.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = respond(&mut conn, 503, "text/plain", b"too many connections");
            return;
        };
        let server = Arc::clone(self);
        // If the thread can't be created, the closure is dropped with the
        // connection and the slot, so nothing is leaked.
        let _ = std::thread::Builder::new().spawn(move || {
            let _slot = slot;
            if let Err(e) = server.handle(conn) {
                // A client that hangs up (mpv does, whenever it seeks) isn't
                // news, but a stream that broke upstream is.
                if e.downcast_ref::<StreamBroke>().is_some() {
                    eprintln!("veil: {e:#}");
                }
            }
        });
    }

    /// The path after `/<token>/`, if the token is right. Compared without
    /// stopping at the first wrong byte, so response timing says nothing about it.
    fn token_rest<'a>(&self, path: &'a str) -> Option<&'a str> {
        let p = path.strip_prefix('/')?;
        let (given, rest) = p.split_at_checked(self.token.len())?;
        let rest = rest.strip_prefix('/')?;
        constant_time_eq(given.as_bytes(), self.token.as_bytes()).then_some(rest)
    }

    fn handle(self: &Arc<Self>, mut stream: TcpStream) -> Result<()> {
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let mut r = BufReader::new(Deadline::new(stream.try_clone()?, REQUEST_TIMEOUT));
        let line = match read_request_line(&mut r) {
            Ok(line) => line,
            Err(reject) => return refuse(&mut stream, &mut r, reject),
        };
        // The token is checked before a single header is read, so a client
        // without it can make us read one short line and nothing more.
        if self.token_rest(&line.path).is_none() {
            return refuse(&mut stream, &mut r, Reject::new(404, "not found"));
        }
        let req = match read_request_rest(&mut r, line) {
            Ok(req) => req,
            Err(reject) => return refuse(&mut stream, &mut r, reject),
        };
        let rest = self.token_rest(&req.path).unwrap_or_default();
        let segs: Vec<&str> = rest.split('/').collect();
        match (req.method.as_str(), segs.as_slice()) {
            ("GET" | "HEAD", ["s", sid, kind @ ("v" | "a")]) => {
                self.relay(&mut stream, &req, sid, kind)
            }
            ("GET", ["thumb", id]) => self.thumb(&mut stream, id),
            _ => match self.api(&req, &segs) {
                Ok(Some(v)) => respond(
                    &mut stream,
                    200,
                    "application/json",
                    v.to_string().as_bytes(),
                ),
                Ok(None) => respond(
                    &mut stream,
                    404,
                    "application/json",
                    br#"{"error":"not found"}"#,
                ),
                Err(e) => {
                    let body = json!({ "error": friendly(&format!("{e:#}")) }).to_string();
                    respond(&mut stream, 500, "application/json", body.as_bytes())
                }
            },
        }
    }

    fn api(self: &Arc<Self>, req: &Request, segs: &[&str]) -> Result<Option<Value>> {
        let rt = self.runtime();
        let paths = &self.paths;
        let v = match (req.method.as_str(), segs) {
            ("GET", ["api", "config"]) => self.config_json(),
            ("POST", ["api", "config"]) => {
                let patch: ConfigPatch =
                    serde_json::from_slice(&req.body).context("bad settings")?;
                self.update_config(patch)?;
                self.config_json()
            }
            ("GET", ["api", "search"]) => {
                json!(app::search(&rt, req.param("q")?, req.num("n", 40))?)
            }
            ("GET", ["api", "feed"]) => {
                let _g = lock(&self.store);
                json!(app::feed(
                    &rt,
                    paths,
                    req.flag("refresh"),
                    !req.flag("shorts"),
                    req.num("n", 150)
                )?)
            }
            ("GET", ["api", "channel"]) => json!(app::channel(&rt, paths, req.param("id")?)?),
            ("GET", ["api", "subs"]) => json!(app::load_subs(paths)?.channels),
            ("POST", ["api", "subs", "add"]) => {
                let _g = lock(&self.store);
                let (sub, added) = app::add_sub(&rt, paths, req.param("c")?)?;
                json!({ "sub": sub, "added": added })
            }
            ("POST", ["api", "subs", "rm"]) => {
                let _g = lock(&self.store);
                json!(app::remove_sub(paths, req.param("id")?)?)
            }
            ("POST", ["api", "subs", "import"]) => {
                let _g = lock(&self.store);
                let text = std::str::from_utf8(&req.body).context("the file isn't text")?;
                let (added, total) = app::import_subs(paths, text)?;
                json!({ "added": added, "total": total })
            }
            ("GET", ["api", "history"]) => {
                let mut w = app::load_history(paths)?.watched;
                w.reverse();
                json!(w)
            }
            ("POST", ["api", "history", "clear"]) => {
                let _g = lock(&self.store);
                app::clear_history(paths)?;
                json!({})
            }
            ("POST", ["api", "play"]) => {
                let target = req.param("v")?;
                let v = ids::parse_video(target)
                    .with_context(|| format!("{target:?} isn't a YouTube video"))?;
                json!(self.open(&v)?)
            }
            _ => return Ok(None),
        };
        Ok(Some(v))
    }

    fn config_json(&self) -> Value {
        let rt = self.runtime();
        let proxy = rt.net.proxy();
        let route = match proxy {
            None => "direct",
            Some(TOR_PROXY) => "tor",
            Some(_) => "proxy",
        };
        json!({
            "route": route,
            "proxy": proxy,
            "describe": rt.net.describe(),
            "isolated": rt.net.isolates(),
            "torRunning": net::tor_running(),
            "history": rt.cfg.history,
            "thumbnails": rt.cfg.thumbnails,
            "maxHeight": rt.cfg.max_height,
            "configPath": self.paths.config,
        })
    }

    fn update_config(&self, p: ConfigPatch) -> Result<()> {
        let rt = self.runtime();
        let mut cfg: Config = toml::from_str(&toml::to_string(&rt.cfg)?)?;
        match p.route.as_deref() {
            None => {}
            Some("direct") => cfg.proxy = None,
            Some("tor") => {
                cfg.proxy = Some(TOR_PROXY.into());
                cfg.isolate_circuits = true;
            }
            Some("proxy") => {
                let proxy = p
                    .proxy
                    .filter(|s| !s.trim().is_empty())
                    .context("enter a proxy URL")?;
                cfg.proxy = Some(proxy.trim().into());
            }
            Some(other) => bail!("unknown route {other:?}"),
        }
        if let Some(h) = p.history {
            cfg.history = h;
        }
        if let Some(t) = p.thumbnails {
            cfg.thumbnails = t;
        }
        if let Some(m) = p.max_height {
            cfg.max_height = m.clamp(144, 4320);
        }
        // Validates the proxy before anything is saved.
        let next = Runtime::new(cfg, &self.paths, None, false)?;
        next.cfg.save(&self.paths.config)?;
        *self.runtime.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(next);
        // Thumbnails fetched over the old route stay valid; streams don't.
        lock(&self.sessions).clear();
        Ok(())
    }

    fn thumb(&self, out: &mut TcpStream, id: &str) -> Result<()> {
        if !ids::is_video_id(id) {
            return respond(out, 404, "text/plain", b"not found");
        }
        if let Some(bytes) = lock(&self.thumbs).get(id).cloned() {
            return respond_cached(out, &bytes);
        }
        let rt = self.runtime();
        if !rt.cfg.thumbnails {
            return respond(out, 403, "text/plain", b"thumbnails are off");
        }
        let url = format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg");
        let fetched = rt
            .net
            .client()
            .and_then(|c| Ok(c.get(url).send()?.error_for_status()?.bytes()?));
        match fetched {
            Ok(bytes) => {
                let bytes = Arc::new(bytes.to_vec());
                let mut cache = lock(&self.thumbs);
                if cache.len() >= MAX_THUMBS {
                    cache.clear();
                }
                cache.insert(id.to_string(), Arc::clone(&bytes));
                drop(cache);
                respond_cached(out, &bytes)
            }
            Err(e) => respond(out, 502, "text/plain", format!("{e:#}").as_bytes()),
        }
    }

    fn relay(&self, out: &mut TcpStream, req: &Request, sid: &str, kind: &str) -> Result<()> {
        let Some(s) = self.session(sid) else {
            return respond(out, 404, "text/plain", b"stream expired");
        };
        let url = match kind {
            "v" => s.video.as_str(),
            _ => match &s.audio {
                Some(a) => a.as_str(),
                None => return respond(out, 404, "text/plain", b"no separate audio"),
            },
        };
        let method = if req.method == "HEAD" {
            Method::HEAD
        } else {
            Method::GET
        };
        let mut rb = s.client.request(method, url);
        for (k, v) in &s.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let Some(r) = req.header("range") {
            rb = rb.header(RANGE, r);
        }
        let mut up = match rb.send() {
            Ok(r) => r,
            Err(e) => return respond(out, 502, "text/plain", format!("{e:#}").as_bytes()),
        };
        let status = up.status();
        // The format selector only picks plain downloads, but if a manifest
        // still turns up, relaying it would hand mpv absolute segment URLs
        // that it fetches itself, around the route. Refuse it instead.
        if up
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(is_playlist_type)
        {
            return respond(
                out,
                502,
                "text/plain",
                b"the stream is a playlist (HLS or DASH), which veil can't relay through your route",
            );
        }
        let mut head = format!(
            "HTTP/1.1 {} {}\r\n",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        );
        for name in [CONTENT_TYPE, CONTENT_LENGTH, CONTENT_RANGE] {
            if let Some(v) = up.headers().get(&name).and_then(|v| v.to_str().ok()) {
                head.push_str(&format!("{}: {v}\r\n", name.as_str()));
            }
        }
        head.push_str(
            "Accept-Ranges: bytes\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        );
        out.write_all(head.as_bytes())?;
        if req.method != "HEAD" {
            out.set_write_timeout(Some(RELAY_WRITE_TIMEOUT))?;
            copy_body(&mut up, out)?;
        }
        Ok(())
    }
}

/// Streams the body to the client. mpv drops the connection whenever it
/// seeks, so a failed write ends the copy quietly. A failed *read* from
/// upstream is returned instead: the connection is then closed short of the
/// Content-Length, which mpv reports, rather than looking like a complete body.
fn copy_body(up: &mut impl Read, out: &mut impl Write) -> Result<()> {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = match up.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(StreamBroke(e).into()),
        };
        if out.write_all(&buf[..n]).is_err() {
            return Ok(());
        }
    }
}

/// HLS and DASH manifests, however the server spells the type.
fn is_playlist_type(content_type: &str) -> bool {
    let t = content_type.to_ascii_lowercase();
    t.contains("mpegurl") || t.contains("dash+xml")
}

/// A request the server refuses to read on, with the status to say so.
struct Reject {
    status: u16,
    msg: &'static str,
}

impl Reject {
    const fn new(status: u16, msg: &'static str) -> Self {
        Self { status, msg }
    }
}

/// Answers a request we stopped reading, then closes politely. Closing a
/// socket that still holds unread bytes resets the connection, and the client
/// may then lose the answer we just wrote, so first read (and drop) a bounded
/// amount of what it is still sending.
fn refuse(stream: &mut TcpStream, r: &mut BufReader<Deadline>, reject: Reject) -> Result<()> {
    respond(stream, reject.status, "text/plain", reject.msg.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    r.get_mut().until = Instant::now() + LINGER;
    let _ = io::copy(&mut r.by_ref().take(LINGER_BYTES), &mut io::sink());
    Ok(())
}

impl From<io::Error> for Reject {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            // What a socket read timeout looks like, depending on the platform.
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                Reject::new(408, "request timed out")
            }
            _ => Reject::new(400, "bad request"),
        }
    }
}

/// Reads from a socket, but gives up at a fixed deadline for the whole
/// request, so a client can't hold a thread by sending one byte at a time.
struct Deadline {
    stream: TcpStream,
    until: Instant,
}

impl Deadline {
    fn new(stream: TcpStream, within: Duration) -> Self {
        Self {
            stream,
            until: Instant::now() + within,
        }
    }
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self
            .until
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

struct RequestLine {
    method: String,
    path: String,
    query: HashMap<String, String>,
}

fn read_request_line(r: &mut impl BufRead) -> Result<RequestLine, Reject> {
    let mut line = String::new();
    // `take` is what bounds this: read_line alone would buffer an endless line.
    r.by_ref().take(MAX_REQUEST_LINE).read_line(&mut line)?;
    if !line.ends_with('\n') {
        return Err(if line.len() as u64 >= MAX_REQUEST_LINE {
            Reject::new(414, "request line too long")
        } else {
            Reject::new(400, "malformed request line")
        });
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(Reject::new(400, "malformed request line"));
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    Ok(RequestLine {
        method: method.into(),
        path: path.into(),
        query,
    })
}

fn read_request_rest(r: &mut impl BufRead, line: RequestLine) -> Result<Request, Reject> {
    let mut headers = Vec::new();
    let mut budget = r.by_ref().take(MAX_HEADERS);
    loop {
        let mut h = String::new();
        budget.read_line(&mut h)?;
        if !h.ends_with('\n') {
            return Err(if budget.limit() == 0 {
                Reject::new(431, "headers too large")
            } else {
                Reject::new(400, "incomplete headers")
            });
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADER_COUNT {
            return Err(Reject::new(431, "too many headers"));
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len = match headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        None => 0,
        Some((_, v)) => v
            .parse::<usize>()
            .map_err(|_| Reject::new(400, "bad Content-Length"))?,
    };
    if len > MAX_BODY {
        return Err(Reject::new(413, "body too large"));
    }
    // Grown as the bytes arrive rather than allocated up front, so a
    // Content-Length that is never fulfilled costs nothing.
    let mut body = Vec::new();
    r.by_ref().take(len as u64).read_to_end(&mut body)?;
    if body.len() != len {
        return Err(Reject::new(400, "body shorter than Content-Length"));
    }
    Ok(Request {
        method: line.method,
        path: line.path,
        query: line.query,
        headers,
        body,
    })
}

/// One of the `MAX_CONNECTIONS` places; given back when dropped.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(counter: &Arc<AtomicUsize>) -> Option<Slot> {
        if counter.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            counter.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Slot(Arc::clone(counter)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The upstream side of a relayed stream failed part-way through the body.
#[derive(Debug)]
struct StreamBroke(io::Error);

impl std::fmt::Display for StreamBroke {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the stream broke upstream: {}", self.0)
    }
}

impl std::error::Error for StreamBroke {}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn respond(out: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Content Too Large",
        414 => "URI Too Long",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Bad Gateway",
    };
    write!(
        out,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    out.write_all(body)?;
    Ok(())
}

fn respond_cached(out: &mut TcpStream, jpeg: &[u8]) -> Result<()> {
    write!(
        out,
        "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nCache-Control: private, max-age=86400\r\nConnection: close\r\n\r\n",
        jpeg.len()
    )?;
    out.write_all(jpeg)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn server() -> (Arc<Server>, String, tempdir::Dir) {
        let dir = tempdir::Dir::new();
        let paths = Paths {
            config: dir.0.join("config.toml"),
            data: dir.0.join("data"),
            cache: dir.0.join("cache"),
        };
        let rt = Runtime::new(Config::default(), &paths, None, false).unwrap();
        let s = Server::new(rt, paths);
        let base = s.listen().unwrap();
        (s, base, dir)
    }

    fn addr(base: &str) -> &str {
        base.trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap()
    }

    fn http(base: &str, method: &str, path: &str, body: &str) -> (u16, String) {
        let mut c = TcpStream::connect(addr(base)).unwrap();
        write!(
            c,
            "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        let status = resp[9..12].parse().unwrap();
        (status, resp.split_once("\r\n\r\n").unwrap().1.to_string())
    }

    fn path(base: &str, rest: &str) -> String {
        format!("/{}/{rest}", base.rsplit('/').next().unwrap())
    }

    #[test]
    fn requires_the_token() {
        let (_s, base, _d) = server();
        assert_eq!(http(&base, "GET", "/api/config", "").0, 404);
        assert_eq!(http(&base, "GET", "/wrongtoken/api/config", "").0, 404);
        let (status, body) = http(&base, "GET", &path(&base, "api/config"), "");
        assert_eq!(status, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["route"], "direct");
        assert_eq!(v["history"], false);
    }

    #[test]
    fn settings_validate_persist_and_apply() {
        let (s, base, _d) = server();
        let (status, body) = http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"route":"proxy","proxy":"socks5://x:1"}"#,
        );
        assert_eq!(status, 500);
        assert!(body.contains("socks5h"), "{body}");
        assert!(
            !s.paths.config.exists(),
            "an invalid route must not be saved"
        );

        let (status, body) = http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"route":"tor","history":true}"#,
        );
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (
                v["route"].as_str(),
                v["isolated"].as_bool(),
                v["history"].as_bool()
            ),
            (Some("tor"), Some(true), Some(true))
        );
        let saved = Config::load(&s.paths.config).unwrap();
        assert_eq!(saved.proxy.as_deref(), Some(TOR_PROXY));
    }

    #[test]
    fn offline_endpoints() {
        let (_s, base, _d) = server();
        let csv = "UCsBjURrPoezykLs9EqgamOA,http://x,Fireship\n";
        let (status, body) = http(&base, "POST", &path(&base, "api/subs/import"), csv);
        assert_eq!((status, body.as_str()), (200, r#"{"added":1,"total":1}"#));
        let (_, body) = http(&base, "GET", &path(&base, "api/subs"), "");
        assert!(body.contains("Fireship"));
        let (status, body) = http(&base, "GET", &path(&base, "api/feed"), "");
        assert_eq!(status, 200);
        assert!(body.contains(r#""channels":1"#), "{body}");
        let (status, _) = http(&base, "POST", &path(&base, "api/subs/rm?id=Fireship"), "");
        assert_eq!(status, 200);
        assert_eq!(http(&base, "GET", &path(&base, "s/nope/v"), "").0, 404);
        assert_eq!(
            http(&base, "GET", &path(&base, "thumb/..%2F..%2Fetc"), "").0,
            404
        );
        let (status, body) = http(
            &base,
            "POST",
            &path(&base, "api/play?v=https://evil.example/x"),
            "",
        );
        assert_eq!(status, 500);
        assert!(body.contains("isn't a YouTube video"));
    }

    /// Sends raw bytes and returns the status line's code (0 if none came
    /// back). The server may hang up before reading everything, so write and
    /// read errors are ignored: what matters is what the client got.
    fn raw(base: &str, request: &[u8]) -> u16 {
        let mut c = TcpStream::connect(addr(base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = c.write_all(request);
        let mut resp = Vec::new();
        let _ = c.read_to_end(&mut resp);
        std::str::from_utf8(resp.get(9..12).unwrap_or_default())
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    #[test]
    fn a_client_without_the_token_gets_nothing_more_than_one_line() {
        let (_s, base, _d) = server();
        // The request line alone is enough to be refused: the server doesn't
        // wait for headers that, here, never come.
        assert_eq!(raw(&base, b"GET /nope/api/config HTTP/1.1\r\n"), 404);
    }

    #[test]
    fn request_line_and_header_sizes_are_capped() {
        let (_s, base, _d) = server();
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(9000));
        assert_eq!(raw(&base, long.as_bytes()), 414);

        let token = base.rsplit('/').next().unwrap();
        let head = format!("GET /{token}/api/config HTTP/1.1\r\n");
        // 40 headers of 1 KiB: few enough to count, too many bytes to read.
        let big = format!(
            "{head}{}\r\n",
            format!("X-Pad: {}\r\n", "a".repeat(1000)).repeat(40)
        );
        assert_eq!(raw(&base, big.as_bytes()), 431);
        // Many tiny headers hit the count limit instead.
        let many = format!("{head}{}\r\n", "X: y\r\n".repeat(150));
        assert_eq!(raw(&base, many.as_bytes()), 431);
        // A body bigger than any request the app sends is refused unread.
        let body = format!("POST /{token}/api/config HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n");
        assert_eq!(raw(&base, body.as_bytes()), 413);
        // The caps leave ordinary requests alone.
        let fine = format!("{head}Host: x\r\n\r\n");
        assert_eq!(raw(&base, fine.as_bytes()), 200);
    }

    #[test]
    fn connections_are_capped_and_released() {
        let (_s, base, _d) = server();
        let idle: Vec<TcpStream> = (0..MAX_CONNECTIONS)
            .map(|_| TcpStream::connect(addr(&base)).unwrap())
            .collect();
        // Every slot is taken by a client that says nothing at all.
        assert_eq!(raw(&base, b"GET / HTTP/1.1\r\n\r\n"), 503);
        drop(idle);
        // Hanging up frees the slots again.
        let started = Instant::now();
        let status = loop {
            let status = raw(&base, b"GET / HTTP/1.1\r\n\r\n");
            if status != 503 || started.elapsed() > Duration::from_secs(5) {
                break status;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(status, 404, "slots were not released");
    }

    #[test]
    fn a_trickling_client_cannot_outlast_the_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let trickler = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            // One byte every 50 ms never trips a per-read timeout of 300 ms.
            for _ in 0..40 {
                if c.write_all(b"a").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let (peer, _) = listener.accept().unwrap();
        let started = Instant::now();
        let mut reader = Deadline::new(peer, Duration::from_millis(300));
        let mut buf = [0u8; 1];
        let err = loop {
            if let Err(e) = reader.read(&mut buf) {
                break e;
            }
        };
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        drop(reader);
        trickler.join().unwrap();
    }

    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!("veil-test-{}", super::random_hex()));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
