//! A loopback-only HTTP server: the desktop app's API, a thumbnail fetcher
//! and a video relay.
//!
//! The app and mpv only ever connect to 127.0.0.1. Every request that leaves
//! the machine is made here, through the configured route, so a proxy or Tor
//! covers the video bytes and thumbnails too. Every path starts with a
//! random token, so other local programs and web pages can't use it.
//! Relayed streams forward Range requests, so seeking works over Tor as well.
//!
//! This file is the server itself (its state, its lifecycle, which request
//! goes where); the work is in the modules below it:
//!
//! - `http`: reading requests within limits, answering them, the token.
//! - `api`: the JSON endpoints, settings, streamed search, playing a video.
//! - `jobs`: the feed refresh, one at a time, that requests can join.
//! - `thumbs`: thumbnails, fetched a page at a time.
//! - `relay`: the video relay, chunked and Range-forwarding.

mod api;
mod http;
mod jobs;
mod relay;
#[cfg(test)]
mod testkit;
mod thumbs;

use crate::app::{self, Runtime, Watched};
use crate::ids::VideoRef;
use crate::inflight::{ROUTE_CHANGED, Why};
use crate::store::Paths;
use crate::util::{lock, random_hex};
use anyhow::{Context, Result, bail};
use http::{
    ACCEPT_BACKOFF, Deadline, Pending, REQUEST_LINE_TIMEOUT, REQUEST_TIMEOUT, Reject, Slot,
    Unverified, WRITE_TIMEOUT, read_request_line, read_request_rest, refuse,
};
use relay::{Live, Session, StreamBroke};
use serde::Serialize;
use std::collections::VecDeque;
use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

const MAX_SESSIONS: usize = 4;
const MAX_SEARCH: usize = 60;

pub struct Server {
    runtime: RwLock<Arc<Runtime>>,
    pub paths: Paths,
    token: String,
    base: OnceLock<String>,
    sessions: Mutex<VecDeque<(String, Arc<Session>)>>,
    thumbs: thumbs::Thumbs,
    /// The feed refresh that is running, if one is.
    jobs: jobs::Jobs,
    /// Serializes settings changes.
    settings: Mutex<()>,
    /// Connections with the token being served right now (see `http::MAX_CONNECTIONS`).
    connections: Arc<AtomicUsize>,
    /// Connections that haven't shown the token yet.
    unverified: Arc<Unverified>,
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

/// A video that is resolved and registered for relaying, and what is left to
/// do once the answer has reached whoever asked.
pub struct Opened {
    pub info: PlayInfo,
    /// What to add to the history, if it is on. Not added yet: see `Server::remember`.
    watched: Option<Watched>,
}

impl Server {
    pub fn new(rt: Runtime, paths: Paths) -> Arc<Self> {
        Arc::new(Self {
            runtime: RwLock::new(Arc::new(rt)),
            paths,
            token: random_hex(),
            base: OnceLock::new(),
            sessions: Mutex::new(VecDeque::new()),
            thumbs: thumbs::Thumbs::default(),
            jobs: jobs::Jobs::default(),
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

    /// Whether the route in use is still the one `rt` was made for. Work that
    /// started on `rt` and finishes after this is false belongs to a route the
    /// user left, and its result is dropped.
    fn still_on(&self, rt: &Runtime) -> bool {
        self.runtime().net.same_route(&rt.net)
    }

    /// `work`, which ran on `rt`, unless the route changed meanwhile: then it
    /// is an error, whatever it was. A late reply could otherwise be shown (or
    /// saved) as if it belonged to the route that is in use now.
    fn guarded<T>(&self, rt: &Runtime, work: Result<T>) -> Result<T> {
        if self.still_on(rt) {
            work
        } else {
            bail!(ROUTE_CHANGED)
        }
    }

    /// Ends everything that is running before the engine exits: the streams,
    /// and every yt-dlp process, which is killed and waited for, and its
    /// private directory (the proxy's password is in it) removed. The process
    /// ends right after, so nothing else would do any of that.
    pub fn shutdown(&self) {
        // A settings change that is half-way would swap in a runtime after this.
        let _one_at_a_time = lock(&self.settings);
        for (_, session) in lock(&self.sessions).drain(..) {
            session.cancel();
        }
        self.runtime().yt.calls.close(Why::Exiting);
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

    /// Resolves a video through one pinned identity of the route, registers
    /// relay URLs for it, and adds it to the history (if that is on).
    pub fn open(self: &Arc<Self>, v: &VideoRef) -> Result<PlayInfo> {
        let opened = self.open_until(v, &|| false)?;
        self.remember(&opened);
        Ok(opened.info)
    }

    /// `open`, without the history: that is for the caller to add with
    /// `remember` once the answer has reached whoever asked, so that a video
    /// the window gave up on (and never got the answer for) isn't recorded as
    /// watched. `abort` is asked while yt-dlp runs, and ends it as soon as it
    /// says yes (see `Ytdlp::resolve_until`).
    fn open_until(self: &Arc<Self>, v: &VideoRef, abort: &dyn Fn() -> bool) -> Result<Opened> {
        let base = self.listen()?;
        let rt = self.runtime();
        let yt = rt.pinned_ytdlp();
        let format = rt.cfg.format();
        let s = yt.resolve_until(&v.watch_url(), &format, abort)?;
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
        let watched = rt.cfg.history.then(|| Watched {
            id: v.id.clone(),
            title: s.title.clone(),
            channel: s.channel.clone(),
            at: app::now(),
        });
        Ok(Opened {
            info: PlayInfo {
                id: v.id.clone(),
                title: s.title,
                channel: s.channel,
                channel_id: s.channel_id,
                duration: s.duration,
                start: v.start,
                video: format!("{base}/s/{sid}/v"),
                audio: has_audio.then(|| format!("{base}/s/{sid}/a")),
            },
            watched,
        })
    }

    /// Adds an opened video to the history, if that is on.
    fn remember(&self, opened: &Opened) {
        if let Some(w) = &opened.watched
            && let Err(e) = app::record_history(&self.paths, w.clone())
        {
            // The history is a convenience, and the video is resolved and
            // registered by now: a full disk or a damaged history file
            // is not a reason to refuse to play it.
            note!("qveil: couldn't add this video to the history: {e:#}");
        }
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
        // A session that falls out of the queue is ended, not just forgotten:
        // a player that still holds one of its streams would otherwise carry
        // on pulling the video, with nothing left to stop it.
        while sessions.len() > MAX_SESSIONS {
            if let Some((_, evicted)) = sessions.pop_front() {
                evicted.cancel();
            }
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
                    note!("qveil: {e:#}");
                }
            }
        });
    }

    /// The path after `/<token>/`, if the token is right.
    fn token_rest<'a>(&self, path: &'a str) -> Option<&'a str> {
        http::token_rest(&self.token, path)
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
            ("GET", ["thumb", id]) => self.thumb(&mut stream, &req, id),
            ("GET", ["api", "search"]) if req.flag("stream") => {
                self.search_stream(&mut stream, &req)
            }
            ("GET", ["api", "feed"]) => self.feed(&mut stream, &req),
            ("POST", ["api", "play"]) => self.play(&mut stream, &req),
            _ => self.json_api(&mut stream, &req, &segs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::server::testkit::*;
    use crate::store::testing::TempDir;
    use serde_json::Value;

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
}
