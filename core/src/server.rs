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
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::Duration;

const MAX_BODY: usize = 8 << 20;
const MAX_SESSIONS: usize = 4;
const MAX_THUMBS: usize = 800;

pub struct Server {
    runtime: RwLock<Arc<Runtime>>,
    pub paths: Paths,
    token: String,
    base: OnceLock<String>,
    sessions: Mutex<VecDeque<(String, Arc<Session>)>>,
    thumbs: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    /// Serializes read-modify-write of the subscription, feed and history files.
    store: Mutex<()>,
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
            for conn in listener.incoming().flatten() {
                let server = Arc::clone(&server);
                std::thread::spawn(move || {
                    let _ = server.handle(conn);
                });
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

    fn handle(self: &Arc<Self>, mut stream: TcpStream) -> Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let req = read_request(&stream)?;
        let rest = req
            .path
            .strip_prefix('/')
            .and_then(|p| p.strip_prefix(self.token.as_str()))
            .and_then(|p| p.strip_prefix('/'));
        let Some(rest) = rest else {
            return respond(&mut stream, 404, "text/plain", b"not found");
        };
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
            // mpv drops the connection whenever it seeks; that's not an error.
            let _ = std::io::copy(&mut up, out);
        }
        Ok(())
    }
}

/// HLS and DASH manifests, however the server spells the type.
fn is_playlist_type(content_type: &str) -> bool {
    let t = content_type.to_ascii_lowercase();
    t.contains("mpegurl") || t.contains("dash+xml")
}

fn read_request(stream: &TcpStream) -> Result<Request> {
    let mut r = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        bail!("malformed request line");
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if headers.len() > 100 {
            bail!("too many headers");
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        bail!("body too large");
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok(Request {
        method: method.into(),
        path: path.into(),
        query,
        headers,
        body,
    })
}

fn respond(out: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
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

    fn http(base: &str, method: &str, path: &str, body: &str) -> (u16, String) {
        let addr = base
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap();
        let mut c = TcpStream::connect(addr).unwrap();
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
