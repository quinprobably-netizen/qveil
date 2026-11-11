//! The video relay: what mpv fetches goes through here, to the upstream
//! URL that yt-dlp resolved, over the route the video was opened on.

use super::Server;
use super::http::{Request, respond};
use super::warm::{Piece, Warm};
use crate::errors::friendly;
use crate::ids::VideoRef;
use crate::net::Net;
use crate::util::lock;
use anyhow::{Result, bail};
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use reqwest::{Method, StatusCode};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// A relayed body is different: mpv stops reading while a paused video's
/// cache is full, so a short write timeout would kill a paused video.
const RELAY_WRITE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// googlevideo paces one long Range request to about twice the video's
/// bitrate, from the first byte (0.4 MB/s for a 1080p video, measured), while a
/// bounded range of this size comes at line speed. So a player's open-ended
/// request is fetched upstream as consecutive ranges of this size and handed
/// on as one response. yt-dlp splits its downloads the same way.
pub(super) const CHUNK: u64 = 10 << 20;
/// The next chunk is requested once no more than this is waiting for the
/// player, so it arrives before the player runs out. At most this plus one
/// chunk is held per relayed stream.
const LOW_WATER: usize = 2 << 20;
/// How often a relay waiting on the other side of `Ahead` looks at the cancel flag.
const AHEAD_POLL: Duration = Duration::from_millis(250);

/// One relayed stream. What mpv fetches goes through `live`, which is
/// replaced when the upstream URLs go stale (see `Server::refresh`).
pub(super) struct Session {
    source: VideoRef,
    /// The yt-dlp format selector it was resolved with, to resolve it again the same way.
    format: String,
    /// The route it was opened on, not one pinned identity of it: a fresh
    /// identity is derived from it each time the stream is resolved again.
    pub(super) route: Net,
    pub(super) live: Mutex<Live>,
    /// Held while resolving again, so simultaneous failures share one resolve.
    refreshing: Mutex<()>,
    /// Set when the route changes. A relay that is already copying bytes
    /// would otherwise keep pulling the rest of the video over the route the
    /// user just left (mpv holds one open-ended Range request).
    pub(super) cancelled: AtomicBool,
    /// The start and end of its streams, fetched when it was opened (see `warm`).
    pub(super) warm: Warm,
    /// The client sockets of the relays in flight, so that cancelling can
    /// wake one that is blocked writing to a player that has paused.
    clients: Mutex<Vec<(u64, TcpStream)>>,
}

impl Session {
    pub(super) fn new(source: VideoRef, format: String, route: Net, live: Live) -> Self {
        Session {
            source,
            format,
            route,
            live: Mutex::new(live),
            refreshing: Mutex::new(()),
            cancelled: AtomicBool::new(false),
            warm: Warm::default(),
            clients: Mutex::new(Vec::new()),
        }
    }

    /// Ends the streams of this session: no relay reads another byte from
    /// upstream, and the players' connections are closed.
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.warm.stop();
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
pub(super) struct Live {
    /// Counts resolves, so a waiter can tell that someone else already refreshed.
    pub(super) generation: u32,
    /// Counts the clients put in place of a stalled one since the last
    /// resolve (see `Server::redial`), so that requests that failed on the
    /// same client make one new client between them.
    pub(super) dial: u32,
    pub(super) video: String,
    pub(super) audio: Option<String>,
    pub(super) headers: Vec<(String, String)>,
    pub(super) client: Client,
}

impl Server {
    pub(super) fn relay(
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
        // What the session fetched when it was opened answers the player's
        // first requests without a round trip.
        if let Some(w) = wanted
            && let Some(kept) = s.warm.find(&s, kind, w.start, live.generation)
        {
            return self.relay_kept(out, &s, kind, live, w, kept);
        }
        let mut up = match self.fetch_redialling(&s, &mut live, &req.method, kind, range) {
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
            up = match self.fetch_redialling(&s, &mut live, &req.method, kind, range) {
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
            if let Err(e) = copy_body(&mut up, out, &s.cancelled) {
                // The player asks again, and that has to find a client
                // that isn't stuck on the connection that just stalled.
                self.redial(&s, &live);
                return Err(e);
            }
        }
        Ok(())
    }

    /// `fetch_upstream`, and once more on a new client if the request
    /// timed out or its connection failed (see `redial`). `live` is then
    /// the session's new upstream half.
    fn fetch_redialling(
        &self,
        s: &Session,
        live: &mut Live,
        method: &str,
        kind: &str,
        range: Option<&str>,
    ) -> Result<Option<reqwest::blocking::Response>, String> {
        match fetch_upstream(live, &s.route, method, kind, range) {
            Err(e) => match self.redial(s, live) {
                Some(fresh) => {
                    *live = fresh;
                    fetch_upstream(live, &s.route, method, kind, range)
                }
                None => Err(e),
            },
            done => done,
        }
    }

    /// Puts a new relay client in place of the one `seen` has, after a
    /// request on it timed out or its connection failed, and returns the
    /// session's upstream half with it. HTTP/2 puts all of a session's
    /// requests on one connection, and one that stalls without closing (a
    /// Tor circuit that stops moving) is never given up by the client: every
    /// later request would wait out the idle timeout on it. The new client
    /// dials again, and over SOCKS with new credentials, so Tor builds a new
    /// circuit; a different exit then gets a 403, which resolves the stream
    /// again as usual. Requests that failed on the same client (video and
    /// audio together) make one new client between them: the first to
    /// get here makes it, and the others find it. The lookup kept for
    /// playing the video again holds the old client, so it is dropped.
    /// `None` when the session is over or the route has changed.
    pub(super) fn redial(&self, s: &Session, seen: &Live) -> Option<Live> {
        if s.cancelled.load(Ordering::Acquire) || !self.runtime().net.same_route(&s.route) {
            return None;
        }
        let fresh = {
            let mut live = lock(&s.live);
            if live.generation != seen.generation || live.dial != seen.dial {
                return Some(live.clone());
            }
            live.client = s.route.pinned().relay_client().ok()?;
            live.dial += 1;
            live.clone()
        };
        self.lookups.forget(&s.source.id);
        Some(fresh)
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
        self.serve_ahead(
            out,
            s,
            kind,
            live,
            Ahead::default(),
            ctype,
            plan,
            Some(first),
        )
    }

    /// Answers a range that starts inside `kept`: its bytes first, from
    /// memory, while the rest is fetched upstream from where they end.
    fn relay_kept(
        self: &Arc<Self>,
        out: &mut TcpStream,
        s: &Arc<Session>,
        kind: &str,
        live: Live,
        w: Wanted,
        kept: Piece,
    ) -> Result<()> {
        let end = w.end.map_or(kept.total - 1, |e| e.min(kept.total - 1));
        let plan = Plan {
            start: w.start,
            first_end: end.min(kept.last()),
            end,
            total: kept.total,
        };
        let ahead = Ahead::default();
        {
            let mut st = lock(&ahead.state);
            let from = (plan.start - kept.start) as usize;
            let to = (plan.first_end - kept.start) as usize;
            st.blocks.push_back(kept.bytes[from..=to].to_vec());
            st.bytes = to + 1 - from;
            if plan.first_end == plan.end {
                st.done = Some(Ok(()));
            }
        }
        self.serve_ahead(out, s, kind, live, ahead, kept.content_type, plan, None)
    }

    /// Sends the head of a 206 for `plan` and then what `ahead` gets. The
    /// bytes after `plan.first_end` are fetched upstream by a second thread,
    /// starting with the body of `first` if there is one (it carries
    /// `plan.start..=plan.first_end`), or else with what follows what `ahead`
    /// already holds.
    #[allow(clippy::too_many_arguments)]
    fn serve_ahead(
        self: &Arc<Self>,
        out: &mut TcpStream,
        s: &Arc<Session>,
        kind: &str,
        live: Live,
        ahead: Ahead,
        ctype: Option<String>,
        plan: Plan,
        first: Option<reqwest::blocking::Response>,
    ) -> Result<()> {
        let ahead = Arc::new(ahead);
        if first.is_some() || plan.first_end < plan.end {
            let fetcher = Fetcher {
                server: Arc::clone(self),
                session: Arc::clone(s),
                kind: kind.to_string(),
                live,
                ahead: Arc::clone(&ahead),
                pos: if first.is_some() {
                    plan.start
                } else {
                    plan.first_end + 1
                },
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
            dial: 0,
            video: streams.video,
            audio: streams.audio,
            headers: streams.headers,
            client: yt.net.relay_client()?,
        };
        *lock(&s.live) = fresh.clone();
        // A lookup kept for playing it again has stopped working too.
        self.lookups.forget(&s.source.id);
        Ok(fresh)
    }
}

/// Starts the upstream request for one relayed GET or HEAD, with `range` as
/// its Range header. `None` means there is no separate audio stream. Errors
/// are text for the client, without the URL: it is signed for this address.
pub(super) fn fetch_upstream(
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
pub(super) fn content_range(up: &reqwest::blocking::Response) -> Option<(u64, u64, u64)> {
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
    /// Upstream ended it, or sent more than it was asked for.
    Broke(String),
    /// A read failed: it timed out, or the connection broke.
    Failed(String),
}

impl Fetcher {
    fn run(mut self, first: Option<reqwest::blocking::Response>) {
        let result = self.fetch(first);
        lock(&self.ahead.state).done = Some(result);
        self.ahead.changed.notify_all();
    }

    fn stopped(&self) -> bool {
        self.session.cancelled.load(Ordering::Acquire) || lock(&self.ahead.state).gone
    }

    fn fetch(&mut self, first: Option<reqwest::blocking::Response>) -> Result<(), String> {
        // Where the last connection failed. A connection that drops part-way
        // is picked up again at the byte where it broke, but a second failure
        // at the same byte ends the stream.
        let mut failed_at = None;
        let mut up = match first {
            Some(up) => up,
            None => match self.next(&mut failed_at)? {
                Some(up) => up,
                None => return Ok(()),
            },
        };
        loop {
            match self.take(&mut up) {
                Taken::Stopped => return Ok(()),
                Taken::Complete if self.pos > self.end => return Ok(()),
                Taken::Complete => {}
                Taken::Broke(e) => once_at(self.pos, &mut failed_at, e)?,
                Taken::Failed(e) => {
                    self.redial();
                    once_at(self.pos, &mut failed_at, e)?;
                }
            }
            up = match self.next(&mut failed_at)? {
                Some(up) => up,
                None => return Ok(()),
            };
        }
    }

    /// The response for the next chunk, once there is room for it. `None`
    /// if the player went away or the route changed meanwhile.
    fn next(
        &mut self,
        failed_at: &mut Option<u64>,
    ) -> Result<Option<reqwest::blocking::Response>, String> {
        loop {
            if !self.wait_for_room() {
                return Ok(None);
            }
            match self.request() {
                Ok(next) => return Ok(Some(next)),
                Err(_) if self.stopped() => return Ok(None),
                Err(Failure::Connection(e)) => {
                    self.redial();
                    once_at(self.pos, failed_at, e)?;
                }
                Err(Failure::Final(e)) => return Err(e),
            }
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
                Err(e) => return Taken::Failed(e.to_string()),
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

    /// A new client for the next request, after the last one stalled or
    /// failed (see `Server::redial`).
    fn redial(&mut self) {
        if let Some(fresh) = self.server.redial(&self.session, &self.live) {
            self.live = fresh;
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
pub(super) fn is_playlist_type(content_type: &str) -> bool {
    let t = content_type.to_ascii_lowercase();
    t.contains("mpegurl") || t.contains("dash+xml")
}

/// The upstream side of a relayed stream failed part-way through the body.
#[derive(Debug)]
pub(super) struct StreamBroke(io::Error);

impl std::fmt::Display for StreamBroke {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the stream broke upstream: {}", self.0)
    }
}

impl std::error::Error for StreamBroke {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Runtime;
    use crate::config::Config;
    use crate::server::testkit::*;
    use crate::store::testing::TempDir;
    use crate::util::lock;
    use std::io::Read;
    use std::time::{Duration, Instant};

    fn session_for(rt: &Runtime, url: String) -> Session {
        Session::new(
            test_video(),
            rt.cfg.format(),
            rt.net.clone(),
            Live {
                generation: 0,
                dial: 0,
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

    #[test]
    fn a_player_that_seeks_mid_chunk_costs_no_new_connection_over_http2() {
        use crate::h2up::{H2Upstream, big_bytes_h2};
        let up = H2Upstream::start();
        let (s, base, _d) = server_h2c();
        let sid = add_session(&s, up.url("/big?pace=5"));
        // mpv reads the start of the video and hangs up to seek elsewhere,
        // with most of the first chunk still to come.
        let mut player = start_ranged(&base, &sid);
        let mut buf = vec![0u8; 64 << 10];
        let mut got = 0;
        while got < 100 << 10 {
            got += player.read(&mut buf).unwrap();
        }
        drop(player);
        wait_for("the stream to be cancelled", Duration::from_secs(5), || {
            up.resets() == 1
        });
        // The seek is served over the connection that is already open.
        let from = CHUNK + 3;
        let (status, _, body) = fetch(
            &base,
            "GET",
            &format!("s/{sid}/v"),
            &[("Range", &format!("bytes={from}-{}", from + 99))],
        );
        assert_eq!(status, 206);
        assert_eq!(body, big_bytes_h2(from, from + 100));
        assert_eq!(up.connections(), 1);
        assert_eq!(
            up.ranges(),
            [
                Some(format!("bytes=0-{}", CHUNK - 1)),
                Some(format!("bytes={from}-{}", from + 99)),
            ]
        );
    }

    /// A server whose clients speak HTTP/2 and give up on a connection
    /// that has said nothing for `IDLE`, and whose yt-dlp resolves both
    /// streams to `/big` behind `relay`.
    fn behind_a_freezer(
        relay: &Freezer,
        bin: &TempDir,
        pace: u64,
    ) -> (Arc<Server>, String, TempDir) {
        let later = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 6 * 3600;
        let ytdlp = fake_ytdlp(
            &bin.0,
            &relay.url(&format!("/big?pace={pace}&expire={later}&n=")),
        );
        server_tuned(
            Config {
                ytdlp,
                ..Config::default()
            },
            |rt| {
                rt.net = rt.net.clone().speaking_h2c().idling_after(IDLE);
                rt.yt.net = rt.net.clone();
            },
        )
    }

    const IDLE: Duration = Duration::from_millis(800);

    fn sid_of(base: &str, url: &str) -> String {
        rel(base, url).split('/').nth(1).unwrap().to_string()
    }

    #[test]
    fn a_connection_that_stalls_without_closing_is_dialled_again() {
        use crate::h2up::{H2Upstream, big_bytes_h2};
        let up = H2Upstream::start();
        let relay = Freezer::start(up.addr);
        let bin = TempDir::new();
        let (s, base, _d) = behind_a_freezer(&relay, &bin, 0);
        let info = s.open(&test_video()).unwrap();
        let sid = sid_of(&base, &info.video);
        let range = |kind: &str, from: u64| {
            fetch(
                &base,
                "GET",
                &format!("s/{sid}/{kind}"),
                &[("Range", &format!("bytes={from}-{}", from + 99))],
            )
        };
        let (status, _, body) = range("v", 0);
        assert_eq!((status, body), (206, big_bytes_h2(0, 100)));
        assert_eq!(relay.connections(), 1);

        // The connection stalls, and stays open. The next request waits out
        // the idle timeout once, and is then made again on a new one.
        relay.freeze();
        let started = Instant::now();
        let (status, _, body) = range("v", 100);
        assert_eq!((status, body), (206, big_bytes_h2(100, 200)));
        assert!(started.elapsed() < IDLE * 4, "{:?}", started.elapsed());
        assert_eq!(relay.connections(), 2);
        // The ones after it use the new connection at once.
        let started = Instant::now();
        let (status, _, body) = range("a", 300);
        assert_eq!((status, body), (206, big_bytes_h2(300, 400)));
        assert!(started.elapsed() < IDLE, "{:?}", started.elapsed());
        assert_eq!(relay.connections(), 2);
        // The lookup kept for playing it again held the stalled client: it
        // is looked up again instead.
        s.open(&test_video()).unwrap();
        assert_eq!(runs(&bin.0), 2);
    }

    #[test]
    fn video_and_audio_stalling_together_get_one_new_client() {
        use crate::h2up::{H2Upstream, big_bytes_h2};
        let up = H2Upstream::start();
        let relay = Freezer::start(up.addr);
        let bin = TempDir::new();
        let (s, base, _d) = behind_a_freezer(&relay, &bin, 0);
        let info = s.open(&test_video()).unwrap();
        let sid = sid_of(&base, &info.video);
        let (status, _, _) = fetch(
            &base,
            "GET",
            &format!("s/{sid}/v"),
            &[("Range", "bytes=0-99")],
        );
        assert_eq!(status, 206);
        relay.freeze();
        let both: Vec<_> = ["v", "a"]
            .into_iter()
            .map(|kind| {
                let (base, path) = (base.clone(), format!("s/{sid}/{kind}"));
                std::thread::spawn(move || {
                    fetch(&base, "GET", &path, &[("Range", "bytes=1000-1099")])
                })
            })
            .collect();
        for got in both {
            let (status, _, body) = got.join().unwrap();
            assert_eq!((status, body), (206, big_bytes_h2(1000, 1100)));
        }
        assert_eq!(lock(&s.session(&sid).unwrap().live).dial, 1);
    }

    #[test]
    fn a_stream_that_stalls_part_way_carries_on_over_a_new_connection() {
        use crate::h2up::{H2Upstream, big_bytes_h2};
        let up = H2Upstream::start();
        let relay = Freezer::start(up.addr);
        let bin = TempDir::new();
        // 16 KiB every 5 ms: the first chunk takes a while to come.
        let (s, base, _d) = behind_a_freezer(&relay, &bin, 5);
        let info = s.open(&test_video()).unwrap();
        let sid = sid_of(&base, &info.video);
        let want = 2u64 << 20;
        let player = {
            let (base, path) = (base.clone(), format!("s/{sid}/v"));
            std::thread::spawn(move || {
                fetch(
                    &base,
                    "GET",
                    &path,
                    &[("Range", &format!("bytes=0-{}", want - 1))],
                )
            })
        };
        wait_for("bytes to flow", Duration::from_secs(5), || {
            up.sent() > 256 << 10
        });
        relay.freeze();
        let (status, _, body) = player.join().unwrap();
        assert_eq!(status, 206);
        assert!(body == big_bytes_h2(0, want), "{} bytes", body.len());
        assert_eq!(relay.connections(), 2);
    }

    #[test]
    fn a_warm_up_that_stalls_leaves_a_new_client_for_the_player() {
        use crate::h2up::{H2Upstream, big_bytes_h2};
        let up = H2Upstream::start();
        let relay = Freezer::start(up.addr);
        let bin = TempDir::new();
        let (s, base, _d) = behind_a_freezer(&relay, &bin, 0);
        s.warming.store(true, std::sync::atomic::Ordering::Relaxed);
        s.open(&test_video()).unwrap();
        wait_for("the warm-up", Duration::from_secs(5), || {
            up.ranges().len() >= 2
        });
        // Played again from the kept lookup, on its client, whose connection
        // has stalled meanwhile: the warm-up of the new session waits it out.
        relay.freeze();
        let rt = s.runtime();
        let info = s.open(&test_video()).unwrap();
        assert_eq!(runs(&bin.0), 1);
        let sid = sid_of(&base, &info.video);
        wait_for(
            "the stalled client to be replaced",
            Duration::from_secs(5),
            || {
                s.lookups
                    .get(&test_video().id, &rt.cfg.format(), &rt.net)
                    .is_none()
            },
        );
        // Video and audio, head and tail, all stalled: one new client.
        std::thread::sleep(IDLE);
        assert_eq!(lock(&s.session(&sid).unwrap().live).dial, 1);
        let started = Instant::now();
        let (status, _, body) = fetch(
            &base,
            "GET",
            &format!("s/{sid}/v"),
            &[("Range", "bytes=0-99")],
        );
        assert_eq!((status, body), (206, big_bytes_h2(0, 100)));
        assert!(started.elapsed() < IDLE, "{:?}", started.elapsed());
        assert_eq!(relay.connections(), 2);
    }

    #[test]
    fn a_session_that_falls_out_of_the_queue_is_ended_not_just_forgotten() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let sid = add_session(&s, up.url("/endless?pace=5"));
        let mut player = start_stream(&base, &sid);
        let held = s.session(&sid).unwrap();
        wait_for("bytes to flow", Duration::from_secs(5), || {
            up.gauge.sent.load(std::sync::atomic::Ordering::SeqCst) > 0
        });
        // One more than the queue holds.
        for _ in 0..crate::server::MAX_SESSIONS {
            add_session(&s, up.url("/video"));
        }
        assert!(s.session(&sid).is_none());
        assert!(held.cancelled.load(std::sync::atomic::Ordering::Acquire));
        // The player lets go, and nothing more is pulled upstream.
        wait_for_close(&mut player);
        wait_for("the upstream to be let go", Duration::from_secs(5), || {
            up.gauge.live.load(std::sync::atomic::Ordering::SeqCst) == 0
        });
    }
}
