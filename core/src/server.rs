//! A loopback-only HTTP server: the desktop app's API, a thumbnail fetcher
//! and a video relay.
//!
//! The app and mpv only ever connect to 127.0.0.1. Every request that leaves
//! the machine is made here, through the configured route, so a proxy or Tor
//! covers the video bytes and thumbnails too. Every path starts with a
//! random token, so other local programs and web pages can't use it.
//! Relayed streams forward Range requests, so seeking works over Tor as well.

use crate::app::{self, Refresh, Runtime, Watched};
use crate::config::Config;
use crate::errors::friendly;
use crate::ids::{self, VideoRef};
use crate::net::{self, Net, TOR_PROXY};
use crate::store::Paths;
use crate::video::Video;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant};

const MAX_BODY: usize = 8 << 20;
const MAX_SESSIONS: usize = 4;
const MAX_THUMBS: usize = 800;
const MAX_SEARCH: usize = 60;

// Limits that apply before a client has proven it holds the token, so that a
// local program (or a web page that guessed the port) can't tie up threads or
// memory just by connecting.
const MAX_REQUEST_LINE: u64 = 8 << 10;
const MAX_HEADERS: u64 = 32 << 10;
const MAX_HEADER_COUNT: usize = 100;
/// How many connections that have presented the token are served at once.
const MAX_CONNECTIONS: usize = 64;
/// How many connections that haven't yet are kept open (see `Unverified`).
const MAX_UNVERIFIED: usize = 64;
/// A client has this long to send a request line, and so to show it has the
/// token. The real client sends it the moment it connects.
const REQUEST_LINE_TIMEOUT: Duration = Duration::from_secs(2);
/// Once the token is shown, the client has this long to send the rest of its request.
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
/// googlevideo paces one long Range request to about twice the video's
/// bitrate, from the first byte (0.4 MB/s for a 1080p video, measured), while a
/// bounded range of this size comes at line speed. So a player's open-ended
/// request is fetched upstream as consecutive ranges of this size and handed
/// on as one response. yt-dlp splits its downloads the same way.
const CHUNK: u64 = 10 << 20;
/// The next chunk is requested once no more than this is waiting for the
/// player, so it arrives before the player runs out. At most this plus one
/// chunk is held per relayed stream.
const LOW_WATER: usize = 2 << 20;
/// How often a relay waiting on the other side of `Ahead` looks at the cancel flag.
const AHEAD_POLL: Duration = Duration::from_millis(250);

pub struct Server {
    runtime: RwLock<Arc<Runtime>>,
    pub paths: Paths,
    token: String,
    base: OnceLock<String>,
    sessions: Mutex<VecDeque<(String, Arc<Session>)>>,
    thumbs: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    /// Serializes settings changes.
    settings: Mutex<()>,
    /// Connections with the token being served right now (see `MAX_CONNECTIONS`).
    connections: Arc<AtomicUsize>,
    /// Connections that haven't shown the token yet.
    unverified: Arc<Unverified>,
}

/// One relayed stream. What mpv fetches goes through `live`, which is
/// replaced when the upstream URLs go stale (see `Server::refresh`).
struct Session {
    source: VideoRef,
    /// The yt-dlp format selector it was resolved with, to resolve it again the same way.
    format: String,
    /// The route it was opened on, not one pinned identity of it: a fresh
    /// identity is derived from it each time the stream is resolved again.
    route: Net,
    live: Mutex<Live>,
    /// Held while resolving again, so simultaneous failures share one resolve.
    refreshing: Mutex<()>,
    /// Set when the route changes. A relay that is already copying bytes
    /// would otherwise keep pulling the rest of the video over the route the
    /// user just left (mpv holds one open-ended Range request).
    cancelled: AtomicBool,
    /// The client sockets of the relays in flight, so that cancelling can
    /// wake one that is blocked writing to a player that has paused.
    clients: Mutex<Vec<(u64, TcpStream)>>,
}

impl Session {
    fn new(source: VideoRef, format: String, route: Net, live: Live) -> Self {
        Session {
            source,
            format,
            route,
            live: Mutex::new(live),
            refreshing: Mutex::new(()),
            cancelled: AtomicBool::new(false),
            clients: Mutex::new(Vec::new()),
        }
    }

    /// Ends the streams of this session: no relay reads another byte from
    /// upstream, and the players' connections are closed.
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        for (_, client) in lock(&self.clients).drain(..) {
            // Wakes a relay blocked in a write, and makes the player's
            // reads end, so it doesn't wait on a stream that is gone.
            let _ = client.shutdown(Shutdown::Both);
        }
    }

    /// Lets `cancel` reach this relay's client until the guard is dropped.
    fn attach(&self, client: &TcpStream) -> io::Result<Attached<'_>> {
        let id = rand::random();
        lock(&self.clients).push((id, client.try_clone()?));
        Ok(Attached { session: self, id })
    }
}

struct Attached<'a> {
    session: &'a Session,
    id: u64,
}

impl Drop for Attached<'_> {
    fn drop(&mut self) {
        lock(&self.session.clients).retain(|(id, _)| *id != self.id);
    }
}

/// The upstream half of a session: URLs that only work from the address
/// that resolved them, and the client with that same address.
#[derive(Clone)]
struct Live {
    /// Counts resolves, so a waiter can tell that someone else already refreshed.
    generation: u32,
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
            settings: Mutex::new(()),
            connections: Arc::new(AtomicUsize::new(0)),
            unverified: Arc::new(Unverified::default()),
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
        let format = rt.cfg.format();
        let s = yt.resolve(&v.watch_url(), &format)?;
        let has_audio = s.audio.is_some();
        let session = Session::new(
            v.clone(),
            format,
            rt.net.clone(),
            Live {
                generation: 0,
                video: s.video,
                audio: s.audio,
                headers: s.headers,
                client: yt.net.relay_client()?,
            },
        );
        let sid = self.register(&rt, session)?;
        if rt.cfg.history
            && let Err(e) = app::record_history(
                &self.paths,
                Watched {
                    id: v.id.clone(),
                    title: s.title.clone(),
                    channel: s.channel.clone(),
                    at: app::now(),
                },
            )
        {
            // The history is a convenience, and the video is resolved and
            // registered by now: a full disk or a damaged history file
            // is not a reason to refuse to play it.
            eprintln!("qveil: couldn't add this video to the history: {e:#}");
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

    /// Adds a session that was resolved under `rt`, unless the route has
    /// changed since: a stream resolved on Direct must not become playable
    /// after the user switched to Tor. The check happens under the sessions
    /// lock, which `update_config` also holds while it swaps the runtime and
    /// clears the sessions, so no session can slip in after that clear.
    fn register(&self, rt: &Runtime, session: Session) -> Result<String> {
        let mut sessions = lock(&self.sessions);
        if !self.runtime().net.same_route(&rt.net) {
            bail!("The route changed while opening the video; try again");
        }
        let sid = random_hex();
        sessions.push_back((sid.clone(), Arc::new(session)));
        while sessions.len() > MAX_SESSIONS {
            sessions.pop_front();
        }
        Ok(sid)
    }

    fn session(&self, sid: &str) -> Option<Arc<Session>> {
        lock(&self.sessions)
            .iter()
            .find(|(id, _)| id == sid)
            .map(|(_, s)| Arc::clone(s))
    }

    /// Serves one connection on its own thread. Until it has shown the token
    /// it only counts against the pool of unverified connections, whose
    /// oldest member is closed to make room for a newcomer.
    fn accept(self: &Arc<Self>, conn: TcpStream) {
        let Ok(pending) = Pending::admit(&self.unverified, &conn) else {
            return;
        };
        let server = Arc::clone(self);
        // If the thread can't be created, the closure is dropped with the
        // connection and its place, so nothing is leaked.
        let _ = std::thread::Builder::new().spawn(move || {
            if let Err(e) = server.handle(conn, pending) {
                // A client that hangs up (mpv does, whenever it seeks) isn't
                // news, but a stream that broke upstream is.
                if e.downcast_ref::<StreamBroke>().is_some() {
                    eprintln!("qveil: {e:#}");
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

    fn handle(self: &Arc<Self>, mut stream: TcpStream, pending: Pending) -> Result<()> {
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let mut r = BufReader::new(Deadline::new(stream.try_clone()?, REQUEST_LINE_TIMEOUT));
        let line = match read_request_line(&mut r) {
            Ok(line) => line,
            Err(reject) => return refuse(&mut stream, &mut r, reject),
        };
        // The token is checked before a single header is read, so a client
        // without it can make us read one short line and nothing more.
        if self.token_rest(&line.path).is_none() {
            return refuse(&mut stream, &mut r, Reject::new(404, "not found"));
        }
        // It has the token, so it moves from the pool of strangers to the
        // main one, which only holders of the token can fill.
        let Some(_slot) = Slot::take(&self.connections) else {
            return refuse(
                &mut stream,
                &mut r,
                Reject::new(503, "too many connections"),
            );
        };
        drop(pending);
        r.get_mut().until = Instant::now() + REQUEST_TIMEOUT;
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
            ("GET", ["api", "search"]) if req.flag("stream") => {
                self.search_stream(&mut stream, &req)
            }
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
                // Each result is a line of yt-dlp's output and a card in the
                // window, and `n` comes from the caller.
                let n = req.num("n", 40).clamp(1, MAX_SEARCH);
                json!(app::search(&rt, req.param("q")?, n)?)
            }
            ("GET", ["api", "feed"]) => {
                // `refresh=1` fetches the channels not fetched recently,
                // `&force=1` all of them, and `&channel=UC…` just that one.
                let refresh = match req.query.get("channel").filter(|c| !c.is_empty()) {
                    _ if !req.flag("refresh") => Refresh::Off,
                    Some(id) => Refresh::Channel(id.clone()),
                    None if req.flag("force") => Refresh::Force,
                    None => Refresh::Stale,
                };
                // A refresh can take minutes, and the route can change in the meantime.
                let current = || self.runtime().net.same_route(&rt.net);
                json!(app::feed(
                    &rt,
                    paths,
                    refresh,
                    !req.flag("shorts"),
                    req.num("n", 150),
                    &current
                )?)
            }
            ("GET", ["api", "channel"]) => json!(app::channel(&rt, paths, req.param("id")?)?),
            ("GET", ["api", "subs"]) => json!(app::load_subs(paths)?.channels),
            ("POST", ["api", "subs", "add"]) => {
                let (sub, added) = app::add_sub(&rt, paths, req.param("c")?)?;
                json!({ "sub": sub, "added": added })
            }
            ("POST", ["api", "subs", "rm"]) => {
                json!(app::remove_sub(paths, req.param("id")?)?)
            }
            ("POST", ["api", "subs", "import"]) => {
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
            // Credentials never leave the engine: the window shows this and
            // sends it back unchanged to mean "keep the current proxy".
            "proxy": proxy.map(net::redact),
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
        // One change at a time: each reads the current settings, edits them and saves.
        let _one_at_a_time = lock(&self.settings);
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
                let proxy = proxy.trim();
                // The window is only ever shown the proxy with its credentials
                // redacted, and sends that back unchanged when the field wasn't
                // edited. That means "keep the proxy I have".
                cfg.proxy = match rt.net.proxy() {
                    Some(current) if net::redact(current) == proxy => Some(current.into()),
                    _ => Some(proxy.into()),
                };
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
        let route_changed = !next.net.same_route(&rt.net);
        // Under the sessions lock (see `register`). Streams stay valid across
        // settings like thumbnails or quality, which don't touch the route; a
        // new route would make their upstream URLs play from the wrong address.
        let mut sessions = lock(&self.sessions);
        *self.runtime.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(next);
        if route_changed {
            // Not just forgotten: a relay already copying a body holds its own
            // reference, and would carry on over the old route.
            for (_, session) in sessions.drain(..) {
                session.cancel();
            }
        }
        Ok(())
    }

    /// A search whose results are sent as they come in, one JSON object per
    /// line, so the window can show YouTube's first page of results while
    /// the second is still on its way. A search that fails before its first
    /// result is an ordinary error; one that fails later ends with a line
    /// `{"error": …}`.
    fn search_stream(&self, out: &mut TcpStream, req: &Request) -> Result<()> {
        let rt = self.runtime();
        let n = req.num("n", 40).clamp(1, MAX_SEARCH);
        let mut started = false;
        let mut gone = false;
        let mut each = |v: Video| {
            let mut line = json!(v).to_string();
            line.push('\n');
            if !started {
                started = true;
                line.insert_str(
                    0,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                );
            }
            gone = out.write_all(line.as_bytes()).is_err();
            !gone
        };
        let result = req
            .param("q")
            .and_then(|q| app::search_each(&rt, q, n, &mut each));
        let error = |e: anyhow::Error| friendly(&format!("{e:#}"));
        match result {
            Err(e) if !started => {
                let body = json!({ "error": error(e) }).to_string();
                respond(out, 500, "application/json", body.as_bytes())
            }
            Err(e) => {
                let line = format!("{}\n", json!({ "error": error(e) }));
                out.write_all(line.as_bytes())?;
                Ok(())
            }
            Ok(_) if !started && !gone => respond(out, 200, "application/x-ndjson", b""),
            Ok(_) => Ok(()),
        }
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
            .web_client()
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

    fn relay(
        self: &Arc<Self>,
        out: &mut TcpStream,
        req: &Request,
        sid: &str,
        kind: &str,
    ) -> Result<()> {
        let Some(s) = self.session(sid) else {
            return respond(out, 404, "text/plain", b"stream expired");
        };
        // Attached before looking at the flag, so a cancel is either seen
        // here or finds this client to close.
        let _attached = s.attach(out)?;
        if s.cancelled.load(Ordering::Acquire) {
            return respond(out, 404, "text/plain", b"stream expired");
        }
        // A GET for a byte range (what mpv sends, `bytes=N-` at the start and
        // after every seek) starts with one bounded chunk; anything else is
        // forwarded as it is.
        let wanted = match req.method.as_str() {
            "GET" => req.header("range").and_then(Wanted::parse),
            _ => None,
        };
        let range = match &wanted {
            Some(w) => Some(w.first_chunk()),
            None => req.header("range").map(String::from),
        };
        let range = range.as_deref();
        let mut live = lock(&s.live).clone();
        let mut up = match fetch_upstream(&live, &s.route, &req.method, kind, range) {
            Ok(Some(up)) => up,
            Ok(None) => return respond(out, 404, "text/plain", b"no separate audio"),
            Err(e) => return respond(out, 502, "text/plain", e.as_bytes()),
        };
        if up.status() == StatusCode::FORBIDDEN {
            // googlevideo URLs only work from the address that resolved them,
            // and Tor stops using a circuit for new streams after about ten
            // minutes even when the SOCKS credentials stay the same. On a long
            // video the first 403 is usually that: resolve again under a fresh
            // identity and retry this request once.
            live = match self.refresh(&s, live.generation) {
                Ok(live) => live,
                Err(e) => {
                    let msg = friendly(&format!("{e:#}"));
                    return respond(out, 502, "text/plain", msg.as_bytes());
                }
            };
            up = match fetch_upstream(&live, &s.route, &req.method, kind, range) {
                Ok(Some(up)) => up,
                Ok(None) => return respond(out, 404, "text/plain", b"no separate audio"),
                Err(e) => return respond(out, 502, "text/plain", e.as_bytes()),
            };
        }
        if s.cancelled.load(Ordering::Acquire) {
            // The route changed while the upstream request was in flight.
            return respond(out, 404, "text/plain", b"stream expired");
        }
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
                b"the stream is a playlist (HLS or DASH), which qveil can't relay through your route",
            );
        }
        if let Some(plan) = wanted.and_then(|w| w.plan(&up)) {
            return self.relay_chunked(out, &s, kind, live, up, plan);
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
            copy_body(&mut up, out, &s.cancelled)?;
        }
        Ok(())
    }

    /// Answers a range with one response while a second thread fetches it
    /// upstream chunk by chunk (see `CHUNK`). `first` is the first chunk's
    /// response, already checked by `relay`.
    fn relay_chunked(
        self: &Arc<Self>,
        out: &mut TcpStream,
        s: &Arc<Session>,
        kind: &str,
        live: Live,
        first: reqwest::blocking::Response,
        plan: Plan,
    ) -> Result<()> {
        let ctype = first
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let ahead = Arc::new(Ahead::default());
        let fetcher = Fetcher {
            server: Arc::clone(self),
            session: Arc::clone(s),
            kind: kind.to_string(),
            live,
            ahead: Arc::clone(&ahead),
            pos: plan.start,
            chunk_end: plan.first_end,
            end: plan.end,
            total: plan.total,
        };
        if std::thread::Builder::new()
            .spawn(move || fetcher.run(first))
            .is_err()
        {
            return respond(out, 503, "text/plain", b"too busy to relay this stream");
        }
        // Whatever happens from here on, the fetcher stops once this returns.
        let _gone = Gone(&ahead);
        let mut head = String::from("HTTP/1.1 206 Partial Content\r\n");
        if let Some(t) = ctype {
            head.push_str(&format!("Content-Type: {t}\r\n"));
        }
        head.push_str(&format!(
            "Content-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\n",
            plan.end - plan.start + 1,
            plan.start,
            plan.end,
            plan.total
        ));
        head.push_str(
            "Accept-Ranges: bytes\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        );
        out.write_all(head.as_bytes())?;
        out.set_write_timeout(Some(RELAY_WRITE_TIMEOUT))?;
        ahead.drain_into(out, &s.cancelled)
    }

    /// Resolves the session's video again on a fresh pinned identity and
    /// swaps the result in. `seen` is the generation the failed request used.
    fn refresh(&self, s: &Session, seen: u32) -> Result<Live> {
        // Requests that fail together (video and audio, or mpv reconnecting)
        // queue here and then all use the first one's result.
        let _one = lock(&s.refreshing);
        {
            let live = lock(&s.live);
            if live.generation != seen {
                return Ok(live.clone());
            }
        }
        let rt = self.runtime();
        if !rt.net.same_route(&s.route) {
            // The user switched routes while this stream was playing. Resolving
            // it again would send a request over the route they left.
            bail!("The route changed; open the video again");
        }
        let yt = rt.pinned_ytdlp();
        let streams = yt.resolve(&s.source.watch_url(), &s.format)?;
        let fresh = Live {
            generation: seen + 1,
            video: streams.video,
            audio: streams.audio,
            headers: streams.headers,
            client: yt.net.relay_client()?,
        };
        *lock(&s.live) = fresh.clone();
        Ok(fresh)
    }
}

/// Starts the upstream request for one relayed GET or HEAD, with `range` as
/// its Range header. `None` means there is no separate audio stream. Errors
/// are text for the client, without the URL: it is signed for this address.
fn fetch_upstream(
    live: &Live,
    route: &Net,
    method: &str,
    kind: &str,
    range: Option<&str>,
) -> Result<Option<reqwest::blocking::Response>, String> {
    let url = match kind {
        "v" => live.video.as_str(),
        _ => match &live.audio {
            Some(a) => a.as_str(),
            None => return Ok(None),
        },
    };
    let method = if method == "HEAD" {
        Method::HEAD
    } else {
        Method::GET
    };
    let mut rb = live.client.request(method, url);
    for (k, v) in &live.headers {
        rb = rb.header(k.as_str(), v.as_str());
    }
    if let Some(r) = range {
        rb = rb.header(RANGE, r);
    }
    rb.send()
        .map(Some)
        .map_err(|e| friendly(&route.explain(&format!("{:#}", e.without_url()))))
}

/// The byte range a player asked for: `bytes=start-` or `bytes=start-end`.
/// Suffix ranges and lists of ranges are forwarded as they are instead.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Wanted {
    start: u64,
    end: Option<u64>,
}

impl Wanted {
    fn parse(header: &str) -> Option<Wanted> {
        let spec = header.trim().strip_prefix("bytes=")?.trim();
        let (a, b) = spec.split_once('-')?;
        let start = a.trim().parse().ok()?;
        let end = match b.trim() {
            "" => None,
            b => Some(b.parse().ok()?),
        };
        match end {
            Some(e) if e < start => None,
            _ => Some(Wanted { start, end }),
        }
    }

    /// The Range header of the first upstream request: at most one chunk.
    fn first_chunk(&self) -> String {
        let last = self.start.saturating_add(CHUNK - 1);
        let last = self.end.map_or(last, |e| e.min(last));
        format!("bytes={}-{last}", self.start)
    }

    /// How to serve the rest, if the first chunk's response allows it: a 206
    /// that starts where asked and says how long the whole stream is.
    /// Anything else (a 200, a 416, an unknown length) is forwarded as it is.
    fn plan(&self, first: &reqwest::blocking::Response) -> Option<Plan> {
        if first.status() != StatusCode::PARTIAL_CONTENT {
            return None;
        }
        let (a, b, total) = content_range(first)?;
        if a != self.start || total == 0 {
            return None;
        }
        let end = self.end.map_or(total - 1, |e| e.min(total - 1));
        (b <= end).then_some(Plan {
            start: a,
            first_end: b,
            end,
            total,
        })
    }
}

/// A chunked relay: the bytes `start..=end` of a stream `total` bytes long,
/// of which the first response carries `start..=first_end`.
#[derive(Debug)]
struct Plan {
    start: u64,
    first_end: u64,
    end: u64,
    total: u64,
}

/// `Content-Range: bytes a-b/total`, with a known total.
fn content_range(up: &reqwest::blocking::Response) -> Option<(u64, u64, u64)> {
    let v = up.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    let (span, total) = v.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (a, b) = span.split_once('-')?;
    let (a, b, total) = (a.parse().ok()?, b.parse().ok()?, total.parse().ok()?);
    (a <= b && b < total).then_some((a, b, total))
}

/// The bytes fetched for one relayed range and not yet written to the player,
/// shared by the thread that fetches them (`Fetcher`) and the one that writes
/// them (`drain_into`).
#[derive(Default)]
struct Ahead {
    state: Mutex<AheadState>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct AheadState {
    blocks: VecDeque<Vec<u8>>,
    bytes: usize,
    /// Set by the fetcher when it stops: `Ok` once the range is complete or it
    /// was told to stop, `Err` when the stream broke upstream.
    done: Option<Result<(), String>>,
    /// Set when the player has gone, or the relay ended for any other reason.
    gone: bool,
}

impl Ahead {
    fn wait<'a>(&self, st: MutexGuard<'a, AheadState>) -> MutexGuard<'a, AheadState> {
        match self.changed.wait_timeout(st, AHEAD_POLL) {
            Ok((st, _)) => st,
            Err(e) => e.into_inner().0,
        }
    }

    /// Writes what the fetcher delivers to the player, until the range is
    /// complete. As in `copy_body`, a failed write or a cancel ends it
    /// quietly, and a stream that broke upstream is returned as an error, so
    /// the player is left short of the Content-Length.
    fn drain_into(&self, out: &mut impl Write, cancelled: &AtomicBool) -> Result<()> {
        loop {
            let block = {
                let mut st = lock(&self.state);
                loop {
                    if cancelled.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    if let Some(b) = st.blocks.pop_front() {
                        st.bytes -= b.len();
                        self.changed.notify_all();
                        break b;
                    }
                    match &st.done {
                        Some(Ok(())) => return Ok(()),
                        Some(Err(e)) => return Err(StreamBroke(io::Error::other(e.clone())).into()),
                        None => st = self.wait(st),
                    }
                }
            };
            if out.write_all(&block).is_err() {
                return Ok(());
            }
        }
    }
}

/// Tells the fetcher the player is gone when the relay ends, however it ends.
struct Gone<'a>(&'a Ahead);

impl Drop for Gone<'_> {
    fn drop(&mut self) {
        lock(&self.0.state).gone = true;
        self.0.changed.notify_all();
    }
}

/// Fetches a relayed range upstream, one bounded chunk after another, ahead
/// of the player by at most `LOW_WATER` plus one chunk. Each chunk is read to
/// its end straight away, so its connection goes back to the session's pool
/// for the next chunk (or the next seek) instead of being held half-read
/// while the player's cache is full.
struct Fetcher {
    server: Arc<Server>,
    session: Arc<Session>,
    kind: String,
    live: Live,
    ahead: Arc<Ahead>,
    /// The next byte to fetch.
    pos: u64,
    /// The last byte of the chunk being read.
    chunk_end: u64,
    /// The last byte the player asked for.
    end: u64,
    total: u64,
}

/// How reading one chunk's body ended.
enum Taken {
    Complete,
    Stopped,
    Broke(String),
}

impl Fetcher {
    fn run(mut self, first: reqwest::blocking::Response) {
        let result = self.fetch(first);
        lock(&self.ahead.state).done = Some(result);
        self.ahead.changed.notify_all();
    }

    fn stopped(&self) -> bool {
        self.session.cancelled.load(Ordering::Acquire) || lock(&self.ahead.state).gone
    }

    fn fetch(&mut self, first: reqwest::blocking::Response) -> Result<(), String> {
        let mut up = first;
        // Where the last connection failed. A connection that drops part-way
        // is picked up again at the byte where it broke, but a second failure
        // at the same byte ends the stream.
        let mut failed_at = None;
        loop {
            match self.take(&mut up) {
                Taken::Stopped => return Ok(()),
                Taken::Complete if self.pos > self.end => return Ok(()),
                Taken::Complete => {}
                Taken::Broke(e) => once_at(self.pos, &mut failed_at, e)?,
            }
            up = loop {
                if !self.wait_for_room() {
                    return Ok(());
                }
                match self.request() {
                    Ok(next) => break next,
                    Err(_) if self.stopped() => return Ok(()),
                    Err(Failure::Connection(e)) => once_at(self.pos, &mut failed_at, e)?,
                    Err(Failure::Final(e)) => return Err(e),
                }
            };
        }
    }

    /// Reads the current chunk's body into `ahead`.
    fn take(&mut self, up: &mut reqwest::blocking::Response) -> Taken {
        let mut buf = vec![0u8; 64 << 10];
        loop {
            if self.stopped() {
                return Taken::Stopped;
            }
            let n = match up.read(&mut buf) {
                Ok(0) if self.pos > self.chunk_end => return Taken::Complete,
                Ok(0) => return Taken::Broke("the stream ended early upstream".into()),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) if self.stopped() => return Taken::Stopped,
                Err(e) => return Taken::Broke(e.to_string()),
            };
            // The read can have taken a long time.
            if self.stopped() {
                return Taken::Stopped;
            }
            // Never more than the chunk that was asked for.
            let n = n.min((self.chunk_end + 1 - self.pos) as usize);
            if n == 0 {
                return Taken::Broke("the stream sent more than it was asked for".into());
            }
            self.pos += n as u64;
            let mut st = lock(&self.ahead.state);
            st.bytes += n;
            st.blocks.push_back(buf[..n].to_vec());
            drop(st);
            self.ahead.changed.notify_all();
        }
    }

    /// Waits until the player has used up all but `LOW_WATER` of what is
    /// fetched. False if it went away meanwhile, or the route changed.
    fn wait_for_room(&self) -> bool {
        let mut st = lock(&self.ahead.state);
        loop {
            if st.gone || self.session.cancelled.load(Ordering::Acquire) {
                return false;
            }
            if st.bytes <= LOW_WATER {
                return true;
            }
            st = self.ahead.wait(st);
        }
    }

    /// Requests the next chunk, from `pos`. A 403 resolves the stream again
    /// once, as in `relay`: a long video outlives the address its URLs were
    /// resolved from.
    fn request(&mut self) -> Result<reqwest::blocking::Response, Failure> {
        let last = self.end.min(self.pos + CHUNK - 1);
        let range = format!("bytes={}-{last}", self.pos);
        let route = &self.session.route;
        let get = |live: &Live| match fetch_upstream(live, route, "GET", &self.kind, Some(&range)) {
            Ok(Some(up)) => Ok(up),
            Ok(None) => Err(Failure::Final("no separate audio".into())),
            Err(e) => Err(Failure::Connection(e)),
        };
        let mut up = get(&self.live)?;
        if up.status() == StatusCode::FORBIDDEN {
            self.live = self
                .server
                .refresh(&self.session, self.live.generation)
                .map_err(|e| Failure::Final(friendly(&format!("{e:#}"))))?;
            up = get(&self.live)?;
        }
        let status = up.status();
        if status != StatusCode::PARTIAL_CONTENT {
            return Err(Failure::Final(format!(
                "upstream answered {status} part-way through the stream"
            )));
        }
        match content_range(&up) {
            Some((a, b, total)) if a == self.pos && total == self.total && b <= self.end => {
                self.chunk_end = b;
                Ok(up)
            }
            _ => Err(Failure::Final(
                "upstream sent a different range than the one asked for".into(),
            )),
        }
    }
}

/// Why the next chunk couldn't be requested.
enum Failure {
    /// The connection failed; worth one more try.
    Connection(String),
    /// Upstream answered, and not with the chunk (a 403 that a fresh resolve
    /// didn't cure, say): trying again would only repeat that.
    Final(String),
}

/// Lets the first failure at byte `pos` pass, so it is tried again, and
/// returns the second one in a row as the error that ends the stream.
fn once_at(pos: u64, failed_at: &mut Option<u64>, e: String) -> Result<(), String> {
    if *failed_at == Some(pos) {
        return Err(e);
    }
    *failed_at = Some(pos);
    Ok(())
}

/// Streams the body to the client. mpv drops the connection whenever it
/// seeks, so a failed write ends the copy quietly. A failed *read* from
/// upstream is returned instead: the connection is then closed short of the
/// Content-Length, which mpv reports, rather than looking like a complete body.
/// Once `cancelled` is set (the route changed) the copy ends the same quiet
/// way, short of the Content-Length, and bytes read since are not passed on.
fn copy_body(up: &mut impl Read, out: &mut impl Write, cancelled: &AtomicBool) -> Result<()> {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let n = match up.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) if cancelled.load(Ordering::Acquire) => return Ok(()),
            Err(e) => return Err(StreamBroke(e).into()),
        };
        // The read can have taken a long time.
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
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
        // A socket read timeout surfaces as WouldBlock on Unix; report it as
        // the deadline it is, so callers see one kind for "too slow".
        self.stream.read(buf).map_err(|e| match e.kind() {
            io::ErrorKind::WouldBlock => io::Error::from(io::ErrorKind::TimedOut),
            _ => e,
        })
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

/// The connections that haven't shown the token, kept apart from the main
/// pool so that a program that merely connects can't use up the places the
/// app needs. A full pool doesn't turn the newcomer away, because the app's own
/// connection is a newcomer too: the oldest stranger is closed instead. A
/// connection that has been served ends up out of here within microseconds,
/// so the one that is closed is nearly always one that is just sitting there.
#[derive(Default)]
struct Unverified(Mutex<VecDeque<(u64, TcpStream)>>);

/// A place in `Unverified`, given up when dropped (or when it is closed to make room).
struct Pending {
    pool: Arc<Unverified>,
    id: u64,
}

impl Pending {
    fn admit(pool: &Arc<Unverified>, conn: &TcpStream) -> io::Result<Pending> {
        let id = rand::random();
        let mine = conn.try_clone()?;
        let mut waiting = lock(&pool.0);
        if waiting.len() >= MAX_UNVERIFIED
            && let Some((_, oldest)) = waiting.pop_front()
        {
            // Wakes the thread serving it, which then ends.
            let _ = oldest.shutdown(Shutdown::Both);
        }
        waiting.push_back((id, mine));
        Ok(Pending {
            pool: Arc::clone(pool),
            id,
        })
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        lock(&self.pool.0).retain(|(id, _)| *id != self.id);
    }
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
    use crate::store::testing::TempDir;

    fn server() -> (Arc<Server>, String, TempDir) {
        server_with(Config::default())
    }

    /// A server whose clients may talk plain http, so a local upstream can
    /// stand in for googlevideo.
    fn server_with(cfg: Config) -> (Arc<Server>, String, TempDir) {
        let dir = TempDir::new();
        let paths = Paths {
            config: dir.0.join("config.toml"),
            data: dir.0.join("data"),
            cache: dir.0.join("cache"),
        };
        let mut rt = Runtime::new(cfg, &paths, None, false).unwrap();
        rt.net = rt.net.allowing_http();
        rt.yt.net = rt.net.clone();
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
    fn the_route_label_is_tor_only_for_the_preset_and_isolation_is_reported_truthfully() {
        let (_s, base, _d) = server();
        let set = |body: &str| {
            let (status, body) = http(&base, "POST", &path(&base, "api/config"), body);
            (status, serde_json::from_str::<Value>(&body).unwrap())
        };
        // Tor Browser's port is a proxy like any other: only the preset is "tor".
        let (status, v) = set(r#"{"route":"proxy","proxy":"socks5h://127.0.0.1:9150"}"#);
        assert_eq!(status, 200, "{v}");
        assert_eq!(
            (v["route"].as_str(), v["isolated"].as_bool()),
            (Some("proxy"), Some(true))
        );
        let (_, v) = set(r#"{"route":"proxy","proxy":"socks5h://localhost:9050"}"#);
        assert_eq!(
            (v["route"].as_str(), v["isolated"].as_bool()),
            (Some("proxy"), Some(true))
        );
        // The user's own credentials turn isolation off, and the description says so.
        let (status, v) = set(r#"{"route":"proxy","proxy":"socks5h://me:pw@127.0.0.1:9050"}"#);
        assert_eq!(status, 200, "{v}");
        assert_eq!(
            (v["route"].as_str(), v["isolated"].as_bool()),
            (Some("proxy"), Some(false))
        );
        let said = v["describe"].as_str().unwrap();
        assert!(
            said.contains("not isolated") && !said.contains("pw"),
            "{said}"
        );
        // Sending the redacted proxy back unchanged keeps the credentials.
        let (_, v) = set(&format!(r#"{{"route":"proxy","proxy":{}}}"#, v["proxy"]));
        assert_eq!(v["isolated"].as_bool(), Some(false));
        let (_, v) = set(r#"{"route":"tor"}"#);
        assert_eq!(
            (v["route"].as_str(), v["isolated"].as_bool()),
            (Some("tor"), Some(true))
        );
        assert!(
            v["describe"]
                .as_str()
                .unwrap()
                .contains("fresh credentials per request")
        );
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

    /// Connects `n` clients that never say a word.
    fn idle_clients(base: &str, n: usize) -> Vec<TcpStream> {
        (0..n)
            .map(|_| TcpStream::connect(addr(base)).unwrap())
            .collect()
    }

    #[test]
    fn clients_without_the_token_cannot_lock_the_app_out() {
        let (_s, base, _d) = server();
        // More silent connections than the server has places for.
        let idle = idle_clients(&base, MAX_UNVERIFIED + MAX_CONNECTIONS + 16);
        // The app, which has the token, is served all the same, and at once.
        let started = Instant::now();
        let (status, body) = http(&base, "GET", &path(&base, "api/config"), "");
        assert_eq!(status, 200, "{body}");
        assert!(started.elapsed() < Duration::from_secs(1));

        // And the silent ones don't linger: each is closed, within the short
        // deadline for a request line, not left for the full 15 s.
        let started = Instant::now();
        for mut c in idle {
            c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut said = Vec::new();
            let _ = c.read_to_end(&mut said);
            let said = String::from_utf8_lossy(&said);
            // Closed to make room, or told it was too slow.
            assert!(
                said.is_empty() || said.starts_with("HTTP/1.1 408"),
                "{said}"
            );
        }
        assert!(
            started.elapsed() < REQUEST_LINE_TIMEOUT + Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn the_connections_with_the_token_are_capped_and_released() {
        let (s, base, _d) = server();
        let token = base.rsplit('/').next().unwrap();
        // Connections that show the token but never finish their request,
        // each given time to be moved to the main pool.
        let held: Vec<TcpStream> = (1..=MAX_CONNECTIONS)
            .map(|n| {
                let mut c = TcpStream::connect(addr(&base)).unwrap();
                write!(c, "GET /{token}/api/config HTTP/1.1\r\n").unwrap();
                wait_for(
                    "the connection to be served",
                    Duration::from_secs(5),
                    || s.connections.load(Ordering::Acquire) == n,
                );
                c
            })
            .collect();
        let probe = || {
            raw(
                &base,
                format!("GET /{token}/api/config HTTP/1.1\r\n\r\n").as_bytes(),
            )
        };
        // The main pool is full, so the next one is turned away.
        assert_eq!(probe(), 503);
        drop(held);
        // Hanging up frees the places again.
        let started = Instant::now();
        let status = loop {
            let status = probe();
            if status != 503 || started.elapsed() > Duration::from_secs(5) {
                break status;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(status, 200, "places were not released");
    }

    #[test]
    fn the_rest_of_a_request_has_longer_than_the_request_line() {
        let (_s, base, _d) = server();
        let token = base.rsplit('/').next().unwrap();
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(c, "GET /{token}/api/config HTTP/1.1\r\n").unwrap();
        // Slower than a stranger is allowed to be, but it has shown the token.
        std::thread::sleep(REQUEST_LINE_TIMEOUT + Duration::from_millis(500));
        write!(c, "Host: x\r\n\r\n").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    }

    #[test]
    fn search_results_are_capped() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: crate::store::testing::script(
                &bin.0.join("yt-dlp"),
                // The target is the last thing in the config file.
                r#"while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && tail -n 1 "$2" > "$(dirname "$0")/asked"; shift; done
echo '{"entries":[]}'"#,
            ),
            ..Config::default()
        });
        for (n, asked) in [("1000", "60"), ("0", "1"), ("x", "40"), ("25", "25")] {
            let (status, body) = http(
                &base,
                "GET",
                &path(&base, &format!("api/search?q=a&n={n}")),
                "",
            );
            assert_eq!(status, 200, "{body}");
            let target = std::fs::read_to_string(bin.0.join("asked")).unwrap();
            assert_eq!(target.trim(), format!("'ytsearch{asked}:a'"), "n={n}");
        }
    }

    /// A yt-dlp that prints search results one per line, the way
    /// `--lazy-playlist -j` does: two, a pause, then a third. `tail` is shell
    /// run after them.
    fn streaming_ytdlp(dir: &std::path::Path, tail: &str) -> String {
        let line =
            |id: &str| format!("echo '{{\"id\":\"{id}\",\"title\":\"t\",\"channel\":\"c\"}}'");
        crate::store::testing::script(
            &dir.join("yt-dlp"),
            &format!(
                // Carries on when its output is closed, like a program that
                // doesn't die of SIGPIPE: only being killed stops it.
                "trap '' PIPE\necho \"$*\" > \"$(dirname \"$0\")/args\"\n{}\n{}\nsleep 1\n{}\n{tail}\n",
                line("aaaaaaaaaaa"),
                line("bbbbbbbbbbb"),
                line("ccccccccccc")
            ),
        )
    }

    #[test]
    fn a_streamed_search_sends_each_result_as_it_comes() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: streaming_ytdlp(&bin.0, ""),
            ..Config::default()
        });
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let target = path(&base, "api/search?q=rust&stream=1");
        write!(c, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let started = Instant::now();
        let mut r = BufReader::new(c);
        let mut head = String::new();
        while !head.ends_with("\r\n\r\n") {
            assert!(r.read_line(&mut head).unwrap() > 0);
        }
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("application/x-ndjson"), "{head}");
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        r.read_line(&mut line).unwrap();
        // The first two arrive before yt-dlp's pause is over.
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "{:?}",
            started.elapsed()
        );
        let mut rest = String::new();
        r.read_to_string(&mut rest).unwrap();
        let ids: Vec<String> = line
            .lines()
            .chain(rest.lines())
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .into()
            })
            .collect();
        assert_eq!(ids, ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"]);
        let args = std::fs::read_to_string(bin.0.join("args")).unwrap();
        assert!(args.contains("--lazy-playlist"), "{args}");
    }

    #[test]
    fn a_streamed_search_that_fails_says_so() {
        let bin = TempDir::new();
        // Fails after its results: the stream ends with the error.
        let (_s, base, _d) = server_with(Config {
            ytdlp: streaming_ytdlp(&bin.0, "echo 'ERROR: the network went away' >&2; exit 1"),
            ..Config::default()
        });
        let (status, body) = http(&base, "GET", &path(&base, "api/search?q=a&stream=1"), "");
        assert_eq!(status, 200);
        let last: Value = serde_json::from_str(body.lines().last().unwrap()).unwrap();
        assert!(
            last["error"]
                .as_str()
                .unwrap()
                .contains("network went away"),
            "{body}"
        );
        assert_eq!(body.lines().count(), 4, "{body}");

        // Fails before any: an ordinary error.
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: crate::store::testing::script(
                &bin.0.join("yt-dlp"),
                "echo 'ERROR: nope' >&2; exit 1",
            ),
            ..Config::default()
        });
        let (status, body) = http(&base, "GET", &path(&base, "api/search?q=a&stream=1"), "");
        assert_eq!(status, 500);
        assert!(body.contains("nope"), "{body}");
    }

    #[test]
    fn a_streamed_search_stops_yt_dlp_when_the_window_goes_away() {
        let bin = TempDir::new();
        let done = bin.0.join("finished");
        let (_s, base, _d) = server_with(Config {
            ytdlp: streaming_ytdlp(
                &bin.0,
                &format!(
                    "for i in 1 2 3 4 5; do echo '{{\"id\":\"ddddddddddd\",\"title\":\"t\"}}'; sleep 0.1; done; sleep 2; touch '{}'",
                    done.display()
                ),
            ),
            ..Config::default()
        });
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        let target = path(&base, "api/search?q=rust&stream=1");
        write!(c, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut first = [0u8; 64];
        let _ = c.read(&mut first).unwrap();
        drop(c);
        // The later results find nobody to read them, and yt-dlp is stopped
        // instead of carrying on.
        std::thread::sleep(Duration::from_millis(4500));
        assert!(!done.exists(), "yt-dlp ran to the end");
    }

    #[test]
    fn a_trickling_client_cannot_outlast_the_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let trickler = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            // One byte every 50 ms never trips a per-read timeout of 300 ms,
            // and the trickle lasts 10 s, far past the deadline.
            for _ in 0..200 {
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
        // Generous for loaded CI machines, yet well short of the 10 s trickle.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        drop(reader);
        trickler.join().unwrap();
    }

    /// 100 bytes of `abc…zabc…`, easy to recognise a range of.
    fn payload() -> Vec<u8> {
        (0..100u8).map(|i| b'a' + i % 26).collect()
    }

    type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

    /// What the upstream's endless streams are doing.
    #[derive(Default)]
    struct Gauge {
        /// Streams still being written.
        live: AtomicUsize,
        /// Bytes written so far, over all of them.
        sent: AtomicUsize,
        /// How many more `/big` responses to cut off half-way.
        cuts: Mutex<usize>,
    }

    /// A tiny stand-in for googlevideo on a local port. It records every
    /// request target and Range header it gets.
    struct Upstream {
        addr: std::net::SocketAddr,
        seen: Seen,
        gauge: Arc<Gauge>,
    }

    impl Upstream {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let seen = Seen::default();
            let log = Arc::clone(&seen);
            let gauge = Arc::new(Gauge::default());
            let counted = Arc::clone(&gauge);
            std::thread::spawn(move || {
                for conn in listener.incoming().flatten() {
                    let (log, gauge) = (Arc::clone(&log), Arc::clone(&counted));
                    std::thread::spawn(move || upstream_serve(conn, &log, &gauge));
                }
            });
            Upstream { addr, seen, gauge }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}{path}", self.addr)
        }

        fn requests(&self) -> Vec<(String, Option<String>)> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn upstream_serve(
        mut conn: TcpStream,
        log: &Mutex<Vec<(String, Option<String>)>>,
        gauge: &Gauge,
    ) {
        let mut r = BufReader::new(conn.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let head_only = line.starts_with("HEAD");
        let mut range = None;
        loop {
            let mut h = String::new();
            r.read_line(&mut h).unwrap();
            if h.trim().is_empty() {
                break;
            }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("range:") {
                range = Some(v.trim().to_string());
            }
        }
        log.lock().unwrap().push((target.clone(), range.clone()));
        let body = payload();
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        if path == "/big" {
            return big(conn, query, range.as_deref(), head_only, gauge);
        }
        if path == "/endless" {
            // A video that never ends, 16 KiB at a time, `pace` ms apart, until
            // the other side hangs up.
            let pace: u64 = query
                .strip_prefix("pace=")
                .and_then(|p| p.parse().ok())
                .unwrap_or(0);
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 1000000000000\r\nConnection: close\r\n\r\n"
            );
            gauge.live.fetch_add(1, Ordering::SeqCst);
            let chunk = [b'x'; 16 << 10];
            while conn.write_all(&chunk).is_ok() {
                gauge.sent.fetch_add(chunk.len(), Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(pace));
            }
            gauge.live.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        let mut send = |status: &str, headers: &str, body: &[u8], claimed: usize| {
            let _ = write!(
                conn,
                "HTTP/1.1 {status}\r\n{headers}Content-Length: {claimed}\r\nConnection: close\r\n\r\n"
            );
            if !head_only {
                let _ = conn.write_all(body);
            }
        };
        match path {
            // Like a URL from a retired Tor circuit: `gen=1` is the first
            // resolve of a stream, and anything under `stale=` is never good.
            "/video" if query == "gen=1" || query.starts_with("stale=") => {
                send("403 Forbidden", "", b"denied", 6)
            }
            "/video" => match range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
                Some(r) => {
                    let (a, b) = r.split_once('-').unwrap();
                    let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
                    send(
                        "206 Partial Content",
                        &format!("Content-Type: video/mp4\r\nContent-Range: bytes {a}-{b}/100\r\n"),
                        &body[a..=b],
                        b - a + 1,
                    );
                }
                None => send("200 OK", "Content-Type: video/mp4\r\n", &body, 100),
            },
            "/hls" => send(
                "200 OK",
                "Content-Type: application/vnd.apple.mpegurl\r\n",
                b"#EXTM3U\nhttps://rr1.googlevideo.com/seg1.ts\n",
                43,
            ),
            "/dash" => send(
                "200 OK",
                "Content-Type: application/dash+xml\r\n",
                b"<MPD><BaseURL>https://rr1.googlevideo.com/</BaseURL></MPD>",
                57,
            ),
            // Promises 100 bytes, sends 10 and hangs up.
            "/cut" => send("200 OK", "Content-Type: video/mp4\r\n", &body[..10], 100),
            _ => send("404 Not Found", "", b"", 0),
        }
    }

    /// The size of `/big` unless asked otherwise: two chunks and a bit, so a
    /// whole read crosses two chunk boundaries.
    const BIG: u64 = 2 * CHUNK + 12_345;

    /// Byte `i` of `/big`. 251 is prime, so the pattern never lines up with
    /// a chunk boundary and a byte lost or repeated there shows.
    fn big_byte(i: u64) -> u8 {
        (i % 251) as u8
    }

    fn big_bytes(from: u64, to: u64) -> Vec<u8> {
        (from..to).map(big_byte).collect()
    }

    /// A long video on the fake googlevideo, answering Range requests like
    /// the real one. In the query: `size=` its length, `pace=` ms between
    /// 16 KiB writes, `gen=1` a URL that has gone stale once the first chunk
    /// is through (a retired Tor circuit), `stale=` one that is never good
    /// again after that, and `dead=1` one whose connections die at once after
    /// the first chunk. `Gauge::cuts` makes that many responses stop half-way.
    fn big(mut conn: TcpStream, query: &str, range: Option<&str>, head_only: bool, gauge: &Gauge) {
        let param = |k: &str| {
            query
                .split('&')
                .find_map(|kv| kv.strip_prefix(k)?.strip_prefix('='))
        };
        let size: u64 = param("size").and_then(|s| s.parse().ok()).unwrap_or(BIG);
        let pace: u64 = param("pace").and_then(|s| s.parse().ok()).unwrap_or(0);
        let (a, b) = match range.and_then(|r| r.strip_prefix("bytes=")) {
            Some(r) => {
                let (a, b) = r.split_once('-').unwrap();
                let a: u64 = a.parse().unwrap();
                let b = b.parse().map_or(size - 1, |b: u64| b.min(size - 1));
                (a, b)
            }
            None => (0, size - 1),
        };
        let past_first = a > 0;
        if past_first && (param("gen") == Some("1") || param("stale").is_some()) {
            let _ = write!(
                conn,
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 6\r\nConnection: close\r\n\r\ndenied"
            );
            return;
        }
        if a >= size {
            let _ = write!(
                conn,
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            return;
        }
        let status = if range.is_some() {
            format!("206 Partial Content\r\nContent-Range: bytes {a}-{b}/{size}")
        } else {
            "200 OK".to_string()
        };
        let _ = write!(
            conn,
            "HTTP/1.1 {status}\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            b - a + 1
        );
        if head_only {
            return;
        }
        let mut stop = b + 1;
        if past_first && param("dead").is_some() {
            stop = a;
        }
        let cut = {
            let mut cuts = gauge.cuts.lock().unwrap();
            let cut = *cuts > 0;
            *cuts = cuts.saturating_sub(1);
            cut
        };
        if cut {
            stop = a + (b + 1 - a) / 2;
        }
        gauge.live.fetch_add(1, Ordering::SeqCst);
        let mut at = a;
        while at < stop {
            let n = (16 << 10).min(stop - at);
            let block = big_bytes(at, at + n);
            if conn.write_all(&block).is_err() {
                break;
            }
            gauge.sent.fetch_add(block.len(), Ordering::SeqCst);
            at += n;
            if pace > 0 {
                std::thread::sleep(Duration::from_millis(pace));
            }
        }
        gauge.live.fetch_sub(1, Ordering::SeqCst);
    }

    type Fetched = (u16, HashMap<String, String>, Vec<u8>);

    /// A request with extra headers; returns the status, the lowercased
    /// response headers and the body bytes.
    fn fetch(base: &str, method: &str, rest: &str, headers: &[(&str, &str)]) -> Fetched {
        let mut c = TcpStream::connect(addr(base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let extra: String = headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}\r\n"))
            .collect();
        let target = path(base, rest);
        write!(c, "{method} {target} HTTP/1.1\r\nHost: x\r\n{extra}\r\n").unwrap();
        let mut raw = Vec::new();
        let _ = c.read_to_end(&mut raw);
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("no response head");
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let mut lines = head.lines();
        let status = lines.next().unwrap()[9..12].parse().unwrap();
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        (status, headers, raw[split + 4..].to_vec())
    }

    fn test_video() -> VideoRef {
        VideoRef {
            id: "dQw4w9WgXcQ".into(),
            start: None,
        }
    }

    fn session_for(rt: &Runtime, url: String) -> Session {
        Session::new(
            test_video(),
            rt.cfg.format(),
            rt.net.clone(),
            Live {
                generation: 0,
                video: url,
                audio: None,
                headers: vec![],
                client: rt.net.relay_client().unwrap(),
            },
        )
    }

    /// A session pointing at `url`, registered the way `open` does it.
    fn add_session(s: &Arc<Server>, url: String) -> String {
        let rt = s.runtime();
        s.register(&rt, session_for(&rt, url)).unwrap()
    }

    #[test]
    fn range_requests_are_forwarded_and_answered_as_ranges() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let sid = add_session(&s, up.url("/video"));
        let stream = format!("s/{sid}/v");

        let (status, headers, body) = fetch(&base, "GET", &stream, &[("Range", "bytes=10-19")]);
        assert_eq!(status, 206);
        assert_eq!(headers["content-range"], "bytes 10-19/100");
        assert_eq!(headers["content-length"], "10");
        assert_eq!(body, b"klmnopqrst");
        let seen = up.requests();
        assert_eq!(seen.last().unwrap().1.as_deref(), Some("bytes=10-19"));

        // Without a Range header the whole thing comes back, and the upstream
        // is not sent one that nobody asked for.
        let (status, _, body) = fetch(&base, "GET", &stream, &[]);
        assert_eq!((status, body), (200, payload()));
        assert_eq!(up.requests().last().unwrap().1, None);

        let (status, headers, body) = fetch(&base, "HEAD", &stream, &[]);
        assert_eq!((status, headers["content-length"].as_str()), (200, "100"));
        assert!(body.is_empty());
        assert_eq!(headers["accept-ranges"], "bytes");
    }

    #[test]
    fn playlists_are_refused_not_relayed() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        for path in ["/hls", "/dash"] {
            let sid = add_session(&s, up.url(path));
            let (status, _, body) = fetch(&base, "GET", &format!("s/{sid}/v"), &[]);
            let text = String::from_utf8_lossy(&body);
            assert_eq!(status, 502, "{path}");
            assert!(text.contains("can't relay"), "{text}");
            assert!(
                !text.contains("googlevideo") && !text.contains("EXTM3U"),
                "{text}"
            );
        }
        assert!(is_playlist_type("Application/X-MpegURL; charset=utf-8"));
        assert!(!is_playlist_type("video/mp4"));
    }

    #[test]
    fn a_stream_cut_short_upstream_is_cut_short_for_the_client() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let sid = add_session(&s, up.url("/cut"));
        let (status, headers, body) = fetch(&base, "GET", &format!("s/{sid}/v"), &[]);
        // The client was promised 100 bytes and got 10, so it can tell the
        // stream failed; it isn't passed off as a complete one.
        assert_eq!(status, 200);
        assert_eq!(headers["content-length"], "100");
        assert_eq!(body.len(), 10);
    }

    #[test]
    fn upstream_errors_never_expose_the_signed_url() {
        let (s, base, _d) = server();
        // Nothing listens on port 1, so connecting fails.
        let sid = add_session(&s, "http://127.0.0.1:1/videoplayback?sig=SECRETSIG".into());
        let (status, _, body) = fetch(&base, "GET", &format!("s/{sid}/v"), &[]);
        assert_eq!(status, 502);
        assert!(!String::from_utf8_lossy(&body).contains("SECRETSIG"));
    }

    #[test]
    fn sessions_survive_settings_but_not_a_route_change() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let sid = add_session(&s, up.url("/video"));
        let post = |body: &str| http(&base, "POST", &path(&base, "api/config"), body);
        let works = || fetch(&base, "GET", &format!("s/{sid}/v"), &[]).0;

        // Thumbnails, history and quality have nothing to do with the route.
        for patch in [
            r#"{"thumbnails":false}"#,
            r#"{"history":true,"maxHeight":720}"#,
            r#"{"route":"direct"}"#,
        ] {
            assert_eq!(post(patch).0, 200, "{patch}");
            assert!(s.session(&sid).is_some(), "{patch} dropped the session");
            assert_eq!(works(), 200, "{patch}");
        }

        assert_eq!(post(r#"{"route":"tor"}"#).0, 200);
        assert!(s.session(&sid).is_none());
        assert_eq!(
            works(),
            404,
            "a Direct stream must not outlive the switch to Tor"
        );
    }

    #[test]
    fn a_stream_resolved_on_the_old_route_cannot_be_registered() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        // `open` resolves under this snapshot of the settings…
        let before = s.runtime();
        // …and the user switches to Tor while yt-dlp is still running.
        let post = |body: &str| http(&base, "POST", &path(&base, "api/config"), body);
        assert_eq!(post(r#"{"route":"tor"}"#).0, 200);
        let err = s
            .register(&before, session_for(&before, up.url("/video")))
            .unwrap_err();
        assert!(err.to_string().contains("route changed"), "{err}");
        assert!(lock(&s.sessions).is_empty());

        // A change that leaves the route alone doesn't make `open` fail.
        let before = s.runtime();
        assert_eq!(post(r#"{"thumbnails":false}"#).0, 200);
        let session = session_for(&before, up.url("/video"));
        assert!(s.register(&before, session).is_ok());
    }

    /// Polls until `cond` holds, or panics after `within`.
    fn wait_for(what: &str, within: Duration, mut cond: impl FnMut() -> bool) {
        let started = Instant::now();
        while !cond() {
            assert!(started.elapsed() < within, "gave up waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Opens a relayed stream like a player would, and returns the socket once
    /// the response head has arrived.
    fn start_stream(base: &str, sid: &str) -> TcpStream {
        let mut c = TcpStream::connect(addr(base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            c,
            "GET {} HTTP/1.1\r\nHost: x\r\nRange: bytes=0-\r\n\r\n",
            path(base, &format!("s/{sid}/v"))
        )
        .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            assert_eq!(c.read(&mut b).unwrap(), 1, "the head ended early");
            head.push(b[0]);
        }
        assert!(head.starts_with(b"HTTP/1.1 200"), "{head:?}");
        c
    }

    fn switch_to_tor(base: &str) {
        let (status, body) = http(
            base,
            "POST",
            &path(base, "api/config"),
            r#"{"route":"tor"}"#,
        );
        assert_eq!(status, 200, "{body}");
    }

    #[test]
    fn a_route_switch_ends_a_stream_that_is_playing() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let sid = add_session(&s, up.url("/endless?pace=5"));
        let mut player = start_stream(&base, &sid);
        let mut buf = vec![0u8; 64 << 10];
        let mut got = 0;
        while got < 100 << 10 {
            got += player.read(&mut buf).unwrap();
        }
        assert_eq!(up.gauge.live.load(Ordering::SeqCst), 1);

        switch_to_tor(&base);
        // The player's connection is closed (what it was promised was a
        // terabyte), and the upstream's too, so nothing more is pulled.
        let started = Instant::now();
        let ended = loop {
            match player.read(&mut buf) {
                Ok(0) | Err(_) => break started.elapsed(),
                Ok(_) => assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "the stream carries on"
                ),
            }
        };
        assert!(ended < Duration::from_secs(5), "{ended:?}");
        wait_for(
            "the upstream connection to close",
            Duration::from_secs(5),
            || up.gauge.live.load(Ordering::SeqCst) == 0,
        );
        let sent = up.gauge.sent.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            up.gauge.sent.load(Ordering::SeqCst),
            sent,
            "bytes still flow upstream"
        );
    }

    #[test]
    fn a_route_switch_also_ends_a_stream_whose_player_has_paused() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let sid = add_session(&s, up.url("/endless"));
        // The player reads the head and then nothing: its cache is full. The
        // relay ends up blocked writing to it, so it wouldn't notice a flag.
        let _paused = start_stream(&base, &sid);
        let sent = || up.gauge.sent.load(Ordering::SeqCst);
        let stalled = |ms| {
            let before = sent();
            std::thread::sleep(Duration::from_millis(ms));
            sent() == before
        };
        wait_for("the relay to back up", Duration::from_secs(10), || {
            sent() > 0 && stalled(400)
        });
        assert_eq!(up.gauge.live.load(Ordering::SeqCst), 1);

        switch_to_tor(&base);
        // Without the player reading a single byte, the relay lets go.
        wait_for("the relay to let go", Duration::from_secs(5), || {
            up.gauge.live.load(Ordering::SeqCst) == 0
        });
    }

    /// A server whose every request to YouTube fails at once (nothing listens
    /// on the proxy port), and whose yt-dlp lists a video per channel and
    /// counts its runs. `extra` is shell run first.
    fn feed_server(extra: &str, channels: u32) -> (Arc<Server>, String, TempDir, TempDir) {
        let bin = TempDir::new();
        let (s, base, dir) = server_with(Config {
            proxy: Some("http://127.0.0.1:1".into()),
            ytdlp: crate::store::testing::listing_ytdlp(&bin.0, extra),
            feed_workers: 1,
            ..Config::default()
        });
        for n in 1..=channels {
            let csv = format!("UC{n:022},http://x,Chan{n}\n");
            assert_eq!(
                http(&base, "POST", &path(&base, "api/subs/import"), &csv).0,
                200
            );
        }
        (s, base, dir, bin)
    }

    #[test]
    fn the_feed_api_refreshes_stale_channels_all_of_them_or_one() {
        use crate::store::testing::listing_runs as runs;
        let (_s, base, _d, bin) = feed_server("", 2);
        let feed = |query: &str| http(&base, "GET", &path(&base, &format!("api/feed?{query}")), "");

        // Without `refresh` nothing is fetched.
        assert_eq!(feed("shorts=1").0, 200);
        assert_eq!(runs(&bin.0), 0);
        // `refresh=1` fetches what is stale: here, both.
        let (status, body) = feed("refresh=1");
        assert_eq!(status, 200, "{body}");
        assert_eq!(runs(&bin.0), 2);
        let v: Value = serde_json::from_str(&body).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["channels", "entries", "errors", "fallbacks"]);
        assert_eq!(v["entries"].as_array().unwrap().len(), 2);
        // Straight after, they are fresh.
        assert_eq!(feed("refresh=1").0, 200);
        assert_eq!(runs(&bin.0), 2);
        // `force=1` fetches them all anyway, and `channel=` just the one.
        assert_eq!(feed("refresh=1&force=1").0, 200);
        assert_eq!(runs(&bin.0), 4);
        assert_eq!(feed(&format!("refresh=1&channel=UC{:022}", 2)).0, 200);
        assert_eq!(runs(&bin.0), 5);
        // Both are ignored unless asked to refresh.
        assert_eq!(feed("refresh=0&force=1").0, 200);
        assert_eq!(feed(&format!("channel=UC{:022}", 1)).0, 200);
        assert_eq!(runs(&bin.0), 5);
        // A channel that isn't one of ours is an error, not a quiet no-op.
        let (status, body) = feed(&format!("refresh=1&channel=UC{:022}", 7));
        assert_eq!(status, 500);
        assert!(body.contains("not one of your subscriptions"), "{body}");
        assert_eq!(feed("refresh=1&channel=nope").0, 500);
        assert_eq!(runs(&bin.0), 5);
    }

    #[test]
    fn a_feed_refresh_stops_when_the_route_is_switched() {
        use crate::store::testing::listing_runs as runs;
        // The first channel's lookup takes a moment, and that is when the
        // route is switched; the other four channels must not be fetched.
        let (_s, base, _d, bin) = feed_server("sleep 1", 5);
        let refresh = {
            let base = base.clone();
            std::thread::spawn(move || http(&base, "GET", &path(&base, "api/feed?refresh=1"), ""))
        };
        wait_for("the first lookup to start", Duration::from_secs(10), || {
            runs(&bin.0) == 1
        });
        switch_to_tor(&base);
        let (status, body) = refresh.join().unwrap();
        assert_eq!(status, 200, "{body}");
        assert_eq!(runs(&bin.0), 1, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        let errors = v["errors"].to_string();
        assert!(errors.contains("route changed"), "{errors}");
    }

    #[test]
    fn copy_body_stops_at_once_when_cancelled() {
        struct Chunks {
            sent: usize,
            cancel: Arc<AtomicBool>,
        }
        impl Read for Chunks {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.sent += 1;
                // The route changes while the second read is in progress.
                if self.sent == 2 {
                    self.cancel.store(true, Ordering::Release);
                }
                buf[..4].copy_from_slice(b"data");
                Ok(4)
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let mut up = Chunks {
            sent: 0,
            cancel: Arc::clone(&cancel),
        };
        let mut out = Vec::new();
        copy_body(&mut up, &mut out, &cancel).unwrap();
        // The second chunk arrived after the change, so it isn't passed on.
        assert_eq!((up.sent, out.as_slice()), (2, &b"data"[..]));

        // Already cancelled: not a single read.
        let mut out = Vec::new();
        copy_body(&mut up, &mut out, &cancel).unwrap();
        assert_eq!((up.sent, out.len()), (2, 0));
    }

    /// A yt-dlp that answers with a URL on the local upstream, numbered by how
    /// often it has run (`<url>1`, `<url>2`, …), and counts its runs.
    fn fake_ytdlp(dir: &std::path::Path, url: &str) -> String {
        let script = r#"n=$(cat '@COUNT@' 2>/dev/null || echo 0)
n=$((n + 1))
echo $n > '@COUNT@'
cat <<EOF
{"title":"T","requested_formats":[
 {"url":"@URL@$n","protocol":"https","vcodec":"avc1"},
 {"url":"@URL@$n","protocol":"https","vcodec":"none"}]}
EOF
"#
        .replace("@COUNT@", &dir.join("runs").display().to_string())
        .replace("@URL@", url);
        crate::store::testing::script(&dir.join("yt-dlp"), &script)
    }

    fn runs(dir: &std::path::Path) -> u32 {
        std::fs::read_to_string(dir.join("runs"))
            .map(|s| s.trim().parse().unwrap())
            .unwrap_or(0)
    }

    /// The path of a relay URL below the server's base.
    fn rel<'a>(base: &str, url: &'a str) -> &'a str {
        url.strip_prefix(&format!("{base}/")).unwrap()
    }

    #[test]
    fn a_403_resolves_again_once_and_retries() {
        let up = Upstream::start();
        let bin = TempDir::new();
        // The first resolve gives `…?gen=1`, which the upstream rejects with a
        // 403 like a URL from a retired circuit; the second gives `…?gen=2`.
        let ytdlp = fake_ytdlp(&bin.0, &up.url("/video?gen="));
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let info = s.open(&test_video()).unwrap();
        assert_eq!(runs(&bin.0), 1);

        let (status, _, body) = fetch(
            &base,
            "GET",
            rel(&base, &info.video),
            &[("Range", "bytes=0-4")],
        );
        assert_eq!((status, body.as_slice()), (206, &b"abcde"[..]));
        assert_eq!(runs(&bin.0), 2, "one more resolve, not more");
        let targets: Vec<_> = up.requests().into_iter().map(|r| r.0).collect();
        assert_eq!(targets, ["/video?gen=1", "/video?gen=2"]);
        assert_eq!(
            up.requests()[1].1.as_deref(),
            Some("bytes=0-4"),
            "the Range survives the retry"
        );

        // The session now holds the fresh URLs: the audio half and later
        // requests need no further resolve.
        let audio = rel(&base, info.audio.as_deref().unwrap());
        assert_eq!(fetch(&base, "GET", audio, &[]).0, 200);
        assert_eq!(fetch(&base, "GET", rel(&base, &info.video), &[]).0, 200);
        assert_eq!(runs(&bin.0), 2);
    }

    #[test]
    fn a_history_that_cannot_be_written_does_not_stop_the_video() {
        let up = Upstream::start();
        let bin = TempDir::new();
        let ytdlp = fake_ytdlp(&bin.0, &up.url("/video?gen="));
        let (s, base, _d) = server_with(Config {
            ytdlp,
            history: true,
            ..Config::default()
        });
        // A directory where the history file should be: reading it fails, so
        // recording the video does too.
        std::fs::create_dir_all(s.paths.history()).unwrap();
        assert!(app::record_history(&s.paths, watched()).is_err());
        let info = s.open(&test_video()).unwrap();
        assert_eq!(fetch(&base, "GET", rel(&base, &info.video), &[]).0, 200);

        // With the file writable again, the same call does record it.
        std::fs::remove_dir_all(s.paths.history()).unwrap();
        s.open(&test_video()).unwrap();
        assert_eq!(app::load_history(&s.paths).unwrap().watched.len(), 1);
    }

    fn watched() -> Watched {
        Watched {
            id: "dQw4w9WgXcQ".into(),
            title: "T".into(),
            channel: None,
            at: 0,
        }
    }

    #[test]
    fn a_403_that_persists_is_passed_on_after_one_retry() {
        let up = Upstream::start();
        let bin = TempDir::new();
        let ytdlp = fake_ytdlp(&bin.0, &up.url("/video?stale="));
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let info = s.open(&test_video()).unwrap();
        let (status, _, body) = fetch(&base, "GET", rel(&base, &info.video), &[]);
        assert_eq!((status, body.as_slice()), (403, &b"denied"[..]));
        assert_eq!(runs(&bin.0), 2, "no resolve loop");
    }

    #[test]
    fn a_stream_is_not_resolved_again_on_a_route_the_user_left() {
        let up = Upstream::start();
        let bin = TempDir::new();
        let ytdlp = fake_ytdlp(&bin.0, &up.url("/video?gen="));
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let info = s.open(&test_video()).unwrap();
        let sid = rel(&base, &info.video)
            .split('/')
            .nth(1)
            .unwrap()
            .to_string();
        let held = s.session(&sid).unwrap();
        http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"route":"tor"}"#,
        );
        let err = s.refresh(&held, 0).err().expect("must refuse");
        assert!(err.to_string().contains("route changed"), "{err}");
        assert_eq!(runs(&bin.0), 1, "no request went out on the old route");
    }

    #[test]
    fn the_proxy_is_shown_without_credentials_and_kept_when_sent_back() {
        let (s, base, _d) = server();
        let post = |body: &str| http(&base, "POST", &path(&base, "api/config"), body);
        let (status, body) =
            post(r#"{"route":"proxy","proxy":"http://user:secret@127.0.0.1:3128"}"#);
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["proxy"], "http://***@127.0.0.1:3128");
        assert!(!body.contains("secret"), "{body}");
        let (_, shown) = http(&base, "GET", &path(&base, "api/config"), "");
        assert!(!shown.contains("secret"), "{shown}");
        let saved = |s: &Server| Config::load(&s.paths.config).unwrap().proxy;
        assert_eq!(
            saved(&s).as_deref(),
            Some("http://user:secret@127.0.0.1:3128")
        );

        // The window sends back what it was shown: that keeps the real proxy.
        let (status, _) = post(r#"{"route":"proxy","proxy":"http://***@127.0.0.1:3128"}"#);
        assert_eq!(status, 200);
        assert_eq!(
            saved(&s).as_deref(),
            Some("http://user:secret@127.0.0.1:3128")
        );
        assert_eq!(s.runtime().net.proxy(), saved(&s).as_deref());

        // Anything else replaces it.
        post(r#"{"route":"proxy","proxy":"http://10.0.0.1:8080"}"#);
        assert_eq!(saved(&s).as_deref(), Some("http://10.0.0.1:8080"));
    }

    #[test]
    fn player_ranges_are_parsed_and_anything_else_is_left_alone() {
        let w = |start, end| Some(Wanted { start, end });
        assert_eq!(Wanted::parse("bytes=0-"), w(0, None));
        assert_eq!(Wanted::parse(" bytes=5-9 "), w(5, Some(9)));
        for other in [
            "bytes=-500",
            "bytes=0-1,5-9",
            "bytes=9-5",
            "items=0-",
            "bytes=x-",
        ] {
            assert_eq!(Wanted::parse(other), None, "{other}");
        }
        assert_eq!(
            w(7, None).unwrap().first_chunk(),
            format!("bytes=7-{}", 7 + CHUNK - 1)
        );
        assert_eq!(w(7, Some(9)).unwrap().first_chunk(), "bytes=7-9");
    }

    /// The Range headers the upstream has been sent, in order.
    fn ranges(up: &Upstream) -> Vec<String> {
        up.requests().into_iter().filter_map(|r| r.1).collect()
    }

    #[test]
    fn a_long_range_is_fetched_in_chunks_and_served_as_one_response() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let sid = add_session(&s, up.url("/big"));
        let stream = format!("s/{sid}/v");
        let get = |range: &str| fetch(&base, "GET", &stream, &[("Range", range)]);

        // What mpv sends at the start: the whole video, in one response…
        let (status, headers, body) = get("bytes=0-");
        assert_eq!(status, 206);
        assert_eq!(
            headers["content-range"],
            format!("bytes 0-{}/{BIG}", BIG - 1)
        );
        assert_eq!(headers["content-length"], BIG.to_string());
        assert_eq!(headers["content-type"], "video/mp4");
        assert!(body == big_bytes(0, BIG), "the bytes differ");
        // …fetched as bounded ranges that follow on from each other.
        assert_eq!(
            ranges(&up),
            [
                format!("bytes=0-{}", CHUNK - 1),
                format!("bytes={CHUNK}-{}", 2 * CHUNK - 1),
                format!("bytes={}-{}", 2 * CHUNK, BIG - 1),
            ]
        );

        // After a seek: from wherever the player asks, not from a boundary.
        let from = CHUNK - 7;
        let before = ranges(&up).len();
        let (status, headers, body) = get(&format!("bytes={from}-"));
        assert_eq!(status, 206);
        assert_eq!(
            headers["content-range"],
            format!("bytes {from}-{}/{BIG}", BIG - 1)
        );
        assert!(body == big_bytes(from, BIG), "the bytes differ");
        assert_eq!(
            ranges(&up)[before..],
            [
                format!("bytes={from}-{}", from + CHUNK - 1),
                format!("bytes={}-{}", from + CHUNK, BIG - 1),
            ]
        );

        // A bounded range that spans a chunk boundary, and one that spans more
        // than a chunk.
        let (status, headers, body) = get(&format!("bytes={}-{}", CHUNK - 5, CHUNK + 4));
        assert_eq!(status, 206);
        assert_eq!(headers["content-length"], "10");
        assert_eq!(body, big_bytes(CHUNK - 5, CHUNK + 5));
        let last = 2 * CHUNK + 50;
        let before = ranges(&up).len();
        let (_, headers, body) = get(&format!("bytes=100-{last}"));
        assert_eq!(headers["content-range"], format!("bytes 100-{last}/{BIG}"));
        assert!(body == big_bytes(100, last + 1), "the bytes differ");
        assert_eq!(
            ranges(&up)[before..],
            [
                format!("bytes=100-{}", CHUNK + 99),
                format!("bytes={}-{last}", CHUNK + 100),
            ]
        );

        // An end past the video is cut to it, and a start past it is the
        // upstream's 416, passed on.
        let (_, headers, body) = get(&format!("bytes={}-{}", BIG - 3, BIG + 1000));
        assert_eq!(
            headers["content-range"],
            format!("bytes {}-{}/{BIG}", BIG - 3, BIG - 1)
        );
        assert_eq!(body.len(), 3);
        assert_eq!(get(&format!("bytes={BIG}-")).0, 416);
    }

    #[test]
    fn a_403_part_way_resolves_again_and_carries_on_from_the_same_byte() {
        let up = Upstream::start();
        let bin = TempDir::new();
        // `gen=1` stops working after the first chunk, like a URL whose Tor
        // circuit was retired while the video played; `gen=2` is fresh.
        let ytdlp = fake_ytdlp(&bin.0, &up.url("/big?gen="));
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let info = s.open(&test_video()).unwrap();
        let (status, _, body) = fetch(
            &base,
            "GET",
            rel(&base, &info.video),
            &[("Range", "bytes=0-")],
        );
        assert_eq!(status, 206);
        assert!(body == big_bytes(0, BIG), "the bytes differ");
        assert_eq!(runs(&bin.0), 2, "one more resolve, not more");
        let seen: Vec<_> = up
            .requests()
            .into_iter()
            .map(|(t, r)| format!("{t} {}", r.unwrap_or_default()))
            .collect();
        assert_eq!(
            seen,
            [
                format!("/big?gen=1 bytes=0-{}", CHUNK - 1),
                format!("/big?gen=1 bytes={CHUNK}-{}", 2 * CHUNK - 1),
                format!("/big?gen=2 bytes={CHUNK}-{}", 2 * CHUNK - 1),
                format!("/big?gen=2 bytes={}-{}", 2 * CHUNK, BIG - 1),
            ]
        );
    }

    #[test]
    fn a_403_part_way_that_persists_cuts_the_stream_short_after_one_resolve() {
        let up = Upstream::start();
        let bin = TempDir::new();
        let ytdlp = fake_ytdlp(&bin.0, &up.url("/big?stale="));
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let info = s.open(&test_video()).unwrap();
        let (status, headers, body) = fetch(
            &base,
            "GET",
            rel(&base, &info.video),
            &[("Range", "bytes=0-")],
        );
        // The player was promised the whole video and gets the first chunk,
        // so it can tell the stream broke.
        assert_eq!(status, 206);
        assert_eq!(headers["content-length"], BIG.to_string());
        assert!(body == big_bytes(0, CHUNK), "the bytes differ");
        assert_eq!(runs(&bin.0), 2, "no resolve loop");
    }

    #[test]
    fn a_connection_that_drops_part_way_is_picked_up_where_it_broke() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let sid = add_session(&s, up.url("/big"));
        *up.gauge.cuts.lock().unwrap() = 1;
        let (status, _, body) = fetch(
            &base,
            "GET",
            &format!("s/{sid}/v"),
            &[("Range", "bytes=0-")],
        );
        assert_eq!(status, 206);
        assert!(body == big_bytes(0, BIG), "the bytes differ");
        let half = CHUNK / 2;
        assert_eq!(ranges(&up)[1], format!("bytes={half}-{}", half + CHUNK - 1));
    }

    #[test]
    fn a_stream_that_keeps_breaking_is_cut_short_for_the_client() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let sid = add_session(&s, up.url("/big?dead=1"));
        let (status, headers, body) = fetch(
            &base,
            "GET",
            &format!("s/{sid}/v"),
            &[("Range", "bytes=0-")],
        );
        assert_eq!(status, 206);
        assert_eq!(headers["content-length"], BIG.to_string());
        assert!(body == big_bytes(0, CHUNK), "the bytes differ");
        // The first chunk, then the second twice: one retry, not a loop.
        assert_eq!(ranges(&up).len(), 3, "{:?}", ranges(&up));
    }

    /// Like `start_stream`, but expects a chunked relay's 206.
    fn start_ranged(base: &str, sid: &str) -> TcpStream {
        let mut c = TcpStream::connect(addr(base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            c,
            "GET {} HTTP/1.1\r\nHost: x\r\nRange: bytes=0-\r\n\r\n",
            path(base, &format!("s/{sid}/v"))
        )
        .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            assert_eq!(c.read(&mut b).unwrap(), 1, "the head ended early");
            head.push(b[0]);
        }
        assert!(head.starts_with(b"HTTP/1.1 206"), "{head:?}");
        c
    }

    /// Reads until the relay closes the connection, which must happen within 5 s.
    fn wait_for_close(player: &mut TcpStream) {
        let started = Instant::now();
        let mut buf = vec![0u8; 256 << 10];
        loop {
            match player.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "the stream carries on"
                ),
            }
        }
    }

    /// Waits until the upstream has had no request and sent no byte for a
    /// while, and returns how many requests it had.
    fn settled(up: &Upstream) -> usize {
        let snapshot = || (up.requests().len(), up.gauge.sent.load(Ordering::SeqCst));
        let mut last = snapshot();
        wait_for("the upstream to go quiet", Duration::from_secs(5), || {
            std::thread::sleep(Duration::from_millis(300));
            let now = snapshot();
            std::mem::replace(&mut last, now) == now
        });
        last.0
    }

    #[test]
    fn a_route_switch_ends_a_chunked_stream_part_way_through_a_chunk() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        // Slow enough that the switch lands in the middle of the first chunk.
        let sid = add_session(&s, up.url("/big?pace=5"));
        let mut player = start_ranged(&base, &sid);
        let mut buf = vec![0u8; 64 << 10];
        let mut got = 0;
        while got < 100 << 10 {
            got += player.read(&mut buf).unwrap();
        }
        switch_to_tor(&base);
        wait_for_close(&mut player);
        wait_for(
            "the upstream connection to close",
            Duration::from_secs(5),
            || up.gauge.live.load(Ordering::SeqCst) == 0,
        );
        assert_eq!(settled(&up), 1, "no further chunk was asked for");
    }

    #[test]
    fn a_route_switch_ends_a_chunked_stream_whose_player_has_paused() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let sid = add_session(&s, up.url(&format!("/big?size={}", 5 * CHUNK)));
        // The player reads the head and then nothing. The relay fetches a
        // chunk ahead and waits for room, with nothing being read upstream.
        let mut paused = start_ranged(&base, &sid);
        let requests = settled(&up);
        assert!(
            requests <= 2,
            "{requests} chunks fetched for a paused player"
        );
        switch_to_tor(&base);
        wait_for_close(&mut paused);
        assert_eq!(settled(&up), requests, "fetching went on after the switch");
    }

    #[test]
    fn fetching_stops_when_the_player_goes_away() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        // At this pace the first chunk takes over 3 s to arrive.
        let sid = add_session(&s, up.url("/big?pace=5"));
        let mut player = start_ranged(&base, &sid);
        let mut buf = vec![0u8; 64 << 10];
        let mut got = 0;
        while got < 100 << 10 {
            got += player.read(&mut buf).unwrap();
        }
        // The player seeks elsewhere and hangs up: the rest of the chunk is
        // left unread, not fetched for nobody.
        drop(player);
        wait_for(
            "the upstream connection to close",
            Duration::from_secs(1),
            || up.gauge.live.load(Ordering::SeqCst) == 0,
        );
        assert_eq!(settled(&up), 1, "no further chunk was asked for");
    }
}
