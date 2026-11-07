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
    /// Serializes settings changes.
    settings: Mutex<()>,
    /// Connections being served right now (see `MAX_CONNECTIONS`).
    connections: Arc<AtomicUsize>,
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
        if rt.cfg.history {
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
        // Attached before looking at the flag, so a cancel is either seen
        // here or finds this client to close.
        let _attached = s.attach(out)?;
        if s.cancelled.load(Ordering::Acquire) {
            return respond(out, 404, "text/plain", b"stream expired");
        }
        let mut live = lock(&s.live).clone();
        let mut up = match fetch_upstream(&live, &s.route, req, kind) {
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
            up = match fetch_upstream(&live, &s.route, req, kind) {
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

/// Starts the upstream request for one relayed GET or HEAD, forwarding the
/// Range header. `None` means there is no separate audio stream. Errors are
/// text for the client, without the URL: it is signed for this address.
fn fetch_upstream(
    live: &Live,
    route: &Net,
    req: &Request,
    kind: &str,
) -> Result<Option<reqwest::blocking::Response>, String> {
    let url = match kind {
        "v" => live.video.as_str(),
        _ => match &live.audio {
            Some(a) => a.as_str(),
            None => return Ok(None),
        },
    };
    let method = if req.method == "HEAD" {
        Method::HEAD
    } else {
        Method::GET
    };
    let mut rb = live.client.request(method, url);
    for (k, v) in &live.headers {
        rb = rb.header(k.as_str(), v.as_str());
    }
    if let Some(r) = req.header("range") {
        rb = rb.header(RANGE, r);
    }
    rb.send()
        .map(Some)
        .map_err(|e| friendly(&route.explain(&format!("{:#}", e.without_url()))))
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
    fn wait_for(what: &str, within: Duration, cond: impl Fn() -> bool) {
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
}
