//! The video relay: what mpv fetches goes through here, to the upstream
//! URL that yt-dlp resolved, over the route the video was opened on.

use super::Server;
use super::http::{Request, Watch, respond};
use super::warm::{Head, Meta, Warm};
use crate::errors::friendly;
use crate::ids::VideoRef;
use crate::net::Net;
use crate::util::lock;
use anyhow::{Result, bail};
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use reqwest::{Method, StatusCode};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

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
/// The first chunk of a range that doesn't start at the beginning (a
/// seek).
pub(super) const SEEK_CHUNK: u64 = 512 << 10;
/// What a spare connection's request reads at most: its one byte, or a
/// short error.
const SPARE_MAX: u64 = 64 << 10;
/// How often a session may ask for a spare connection (see
/// `Session::keep_spare`). Each is a `bytes=0-0` request that googlevideo
/// sees, and a player that seeks a lot would otherwise ask for one with
/// every chunk (18 of 43 requests in one session over Tor, measured).
const SPARE_EVERY: Duration = Duration::from_secs(10);
/// How long a spare connection has to be ready (connected, and its one
/// byte in) before it is given up on. Over Tor a new connection answers in
/// under a second (325 to 691 ms, measured), but one dialled while the
/// circuit was busy took 11 s, and was never used.
pub(super) const SPARE_WITHIN: Duration = Duration::from_secs(3);
/// How long a player's request waits for the answer to its upstream
/// request to start, before it is taken to have stalled (see
/// `Server::recover`). Within the idle timeout of a relay client (a
/// minute), which a request that is tried a second time would wait out
/// twice: two minutes of a black window before the player is told.
/// Over Tor an answer takes under a second (300 ms, measured), and a
/// new connection's under two.
const FIRST_WITHIN: Duration = Duration::from_secs(15);
/// How long a connection back in a client's pool is taken to stay there
/// (see `Pool`). The client keeps one 90 s, but googlevideo may close it
/// sooner, and then a spare would be wanted.
const POOLED_FOR: Duration = Duration::from_secs(30);
/// How often a relay waiting on the other side of `Ahead` looks at the cancel flag.
const AHEAD_POLL: Duration = Duration::from_millis(250);
/// How long a resolve again that failed is the answer for the requests on
/// the same resolve, instead of trying again (see `Server::resolve_again`).
const FAILED_FOR: Duration = Duration::from_secs(10);
/// How often a session may put a new client, or a new lookup, in place of
/// one that stalled (see `Server::recover`). A route that keeps stalling
/// ends its streams instead of costing an identity every time.
const RECOVER_EVERY: Duration = Duration::from_secs(10);

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
    /// The spare connection being made ready, and the last (see `keep_spare`).
    pub(super) spare: Mutex<Spare>,
    /// When a stalled client was last replaced (see `Server::recover`).
    recovered: Mutex<Option<Instant>>,
    /// The last resolve again that failed: the generation it was for, when,
    /// and why (see `Server::resolve_again`).
    failed: Mutex<Option<(u32, Instant, String)>>,
    /// `FIRST_WITHIN`, which tests shorten.
    first_within: Duration,
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
            spare: Mutex::default(),
            recovered: Mutex::new(None),
            failed: Mutex::new(None),
            first_within: FIRST_WITHIN,
        }
    }

    /// Whether nobody waits for what this session is doing any more: it was
    /// cancelled, or players were seen waiting (by `seen`, asked again and
    /// again from one thread) and every one of them has gone, hung up or
    /// served. Before the first player, the warm-up is what waits.
    fn nobody_waits(&self, seen: &RefCell<Players>) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return true;
        }
        let clients = lock(&self.clients);
        let seen = &mut *seen.borrow_mut();
        // A `Watch` holds the number of its player's socket, which is only
        // good while that player is attached (and its socket in `clients`).
        seen.watches
            .retain(|id, _| clients.iter().any(|(c, _)| c == id));
        let mut waiting = false;
        for (id, client) in clients.iter() {
            seen.any = true;
            let watch = seen
                .watches
                .entry(*id)
                .or_insert_with(|| Watch::new(client));
            waiting |= !watch.gone();
        }
        seen.any && !waiting
    }

    /// Whether a stalled client may be replaced now, at most once every
    /// `RECOVER_EVERY`; if so, that counts as the one.
    fn may_recover(&self) -> bool {
        let mut at = lock(&self.recovered);
        if at.is_some_and(|t| t.elapsed() < RECOVER_EVERY) {
            return false;
        }
        *at = Some(Instant::now());
        true
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

    /// Makes sure the session's client has a connection to spare, now that
    /// `up` (a chunk's response, over HTTP/1.1) has taken one. A player
    /// that seeks past what it has hangs up on the range it was reading,
    /// and the connection that carried it is closed with the rest of the
    /// chunk unread; the range after the seek then finds this one ready
    /// instead of making a new one, which over Tor is a TCP and a TLS
    /// handshake through the circuit (325 to 691 ms, measured). It is a
    /// one-byte request for the start of the stream, on the same client and
    /// so on the same circuit, read to its end so that its connection goes
    /// back to the client's pool.
    ///
    /// It is a request googlevideo sees, and a connection more, so there is
    /// one at most: none while the pool has an idle connection already (a
    /// spare not used yet, or a chunk read to its end), none while another
    /// is being made ready, none within `SPARE_EVERY` of the last, and one
    /// that isn't ready within `SPARE_WITHIN` is given up on. Over HTTP/2
    /// every request shares one connection, and there is nothing to spare.
    ///
    /// It doesn't go out once the session is cancelled or the route is not
    /// `server`'s any more, which may have happened since it was asked for.
    fn keep_spare(
        self: &Arc<Self>,
        server: &Arc<Server>,
        live: &Live,
        kind: &str,
        up: &reqwest::blocking::Response,
    ) {
        if up.version() >= reqwest::Version::HTTP_2
            || self.cancelled.load(Ordering::Acquire)
            || live.pool.has_idle()
        {
            return;
        }
        {
            let mut spare = lock(&self.spare);
            if spare.dialing || spare.last.is_some_and(|t| t.elapsed() < SPARE_EVERY) {
                return;
            }
            spare.dialing = true;
            spare.last = Some(Instant::now());
        }
        let (s, server) = (Arc::clone(self), Arc::clone(server));
        let (live, kind) = (live.clone(), kind.to_string());
        let spawned = std::thread::Builder::new().spawn(move || {
            // However this ends, the next spare may be asked for.
            let _dialing = Dialing(&s);
            #[cfg(test)]
            {
                let hook = lock(&s.spare).before_send.clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
            if s.cancelled.load(Ordering::Acquire) || !server.runtime().net.same_route(&s.route) {
                return;
            }
            let range = Some("bytes=0-0");
            if let Ok(Some(mut spare)) = send_upstream(
                &live,
                &s.route,
                "GET",
                &kind,
                range,
                Limit::All(SPARE_WITHIN),
            ) {
                let read = io::copy(&mut (&mut spare).take(SPARE_MAX), &mut io::sink());
                // Read to its end, its connection is back in the pool.
                if read.is_ok_and(|n| n < SPARE_MAX) {
                    live.pool.put_back();
                }
            }
        });
        if spawned.is_err() {
            lock(&self.spare).dialing = false;
        }
    }

    /// Lets `cancel` reach this relay's client until the guard is dropped.
    fn attach(&self, client: &TcpStream) -> io::Result<Attached<'_>> {
        let id = rand::random();
        lock(&self.clients).push((id, client.try_clone()?));
        Ok(Attached { session: self, id })
    }
}

/// The players a session's work has seen waiting for it (see
/// `Session::nobody_waits`).
#[derive(Default)]
struct Players {
    watches: HashMap<u64, Watch>,
    any: bool,
}

/// A session's spare connection (see `Session::keep_spare`).
#[derive(Default)]
pub(super) struct Spare {
    /// One is being made ready.
    pub(super) dialing: bool,
    /// When the last was asked for.
    last: Option<Instant>,
    /// Run by the spare's thread right before it sends its request.
    #[cfg(test)]
    pub(super) before_send: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Lets the next spare be asked for once the one being made ready is done
/// with, however that ends.
struct Dialing<'a>(&'a Session);

impl Drop for Dialing<'_> {
    fn drop(&mut self) {
        lock(&self.0.spare).dialing = false;
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
    /// resolve (see `Server::recover`), so that requests that failed on the
    /// same client make one new client between them.
    pub(super) dial: u32,
    pub(super) video: String,
    pub(super) audio: Option<String>,
    pub(super) headers: Vec<(String, String)>,
    pub(super) client: Client,
    /// When `client` last brought anything: a response, or a byte of one.
    pub(super) served: Arc<Served>,
    /// The connections idle in `client`'s pool.
    pub(super) pool: Arc<Pool>,
}

/// The connections idle in a client's pool, as far as the relay can tell
/// (reqwest doesn't say), for `Session::keep_spare`. Over HTTP/1.1 a
/// connection carries one request at a time: every request takes an idle
/// one if there is one, and a response read to its end (a chunk, or a
/// spare's one byte) puts its connection back. One that is cut short is
/// closed. What the warm-up reads isn't counted, so there may be more than
/// it says, which costs a spare too many and never one too few; and one is
/// taken to be gone after `POOLED_FOR`.
#[derive(Default)]
pub(super) struct Pool(Mutex<Vec<Instant>>);

impl Pool {
    /// A response was read to its end, and its connection is idle again.
    pub(super) fn put_back(&self) {
        lock(&self.0).push(Instant::now());
    }

    /// A request is about to go out, on an idle connection if there is one.
    fn take(&self) {
        let mut idle = self.fresh();
        idle.pop();
    }

    pub(super) fn has_idle(&self) -> bool {
        !self.fresh().is_empty()
    }

    fn fresh(&self) -> MutexGuard<'_, Vec<Instant>> {
        let mut idle = lock(&self.0);
        idle.retain(|t| t.elapsed() < POOLED_FOR);
        idle
    }
}

/// When a client last brought anything, which tells a client that is stuck
/// from one request that is.
#[derive(Default)]
pub(super) struct Served(Mutex<Option<Instant>>);

impl Served {
    /// Marks now, and returns it.
    pub(super) fn mark(&self) -> Instant {
        let now = Instant::now();
        *lock(&self.0) = Some(now);
        now
    }

    fn after(&self, t: Instant) -> bool {
        lock(&self.0).is_some_and(|at| at > t)
    }
}

/// Why an upstream request failed before its response came, in words for
/// the player (without the URL: it is signed for this address).
pub(super) struct UpError {
    pub(super) text: String,
    /// It timed out: the connection stalled, rather than broke.
    pub(super) stalled: bool,
}

/// Whether a failed read of a body timed out, rather than its connection
/// breaking.
pub(super) fn stalled_read(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::TimedOut
        || e.get_ref()
            .and_then(|e| e.downcast_ref::<reqwest::Error>())
            .is_some_and(|e| e.is_timeout())
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
            && let Some((head, meta)) = s.warm.find(&s, kind, w.start, live.generation)
        {
            return self.relay_kept(out, &s, kind, live, w, head, meta);
        }
        let mut up = match self.fetch_recovering(&s, &mut live, &req.method, kind, range) {
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
            up = match self.fetch_recovering(&s, &mut live, &req.method, kind, range) {
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
        // that it fetches itself, around the route. Refuse it instead, and
        // anything else that isn't a video either, whatever its type says
        // (an error is passed on as it is: mpv doesn't play what comes with
        // one).
        let refuse = |out: &mut TcpStream| respond(out, 502, "text/plain", NOT_A_STREAM);
        let mut start = Vec::new();
        if status.is_success() {
            if !up_type(&up).is_none_or(is_media_type) {
                return refuse(out);
            }
            if req.method != "HEAD" && content_range(&up).is_none_or(|(a, _, _)| a == 0) {
                let since = Instant::now();
                start = match sniff(&mut up) {
                    Ok(start) => start,
                    Err(e) => {
                        if stalled_read(&e) {
                            self.recover(&s, &live, true, since);
                        }
                        return respond(out, 502, "text/plain", b"the stream broke upstream");
                    }
                };
                if !start.is_empty() {
                    live.served.mark();
                }
                if is_manifest(&start) {
                    return refuse(out);
                }
            }
        }
        if let Some(plan) = wanted.and_then(|w| w.plan(&up)) {
            if self.sparing.load(Ordering::Relaxed) {
                s.keep_spare(self, &live, kind, &up);
            }
            return self.relay_chunked(out, &s, kind, live, up, start, plan);
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
            let mut body = start.as_slice().chain(&mut up);
            if let Err(e) = copy_body(&mut body, out, &s.cancelled, &live.served) {
                // The player asks again, and that has to find a client
                // that isn't stuck on the connection that just stalled.
                if let Some(since) = e
                    .downcast_ref::<StreamBroke>()
                    .and_then(|b| b.stalled_since)
                {
                    self.recover(&s, &live, true, since);
                }
                return Err(e);
            }
        }
        Ok(())
    }

    /// `fetch_upstream`, and once more if that failed: on the same client,
    /// or on what `recover` puts in place of a stalled one. `live` is then
    /// the session's upstream half that was used.
    fn fetch_recovering(
        &self,
        s: &Session,
        live: &mut Live,
        method: &str,
        kind: &str,
        range: Option<&str>,
    ) -> Result<Option<reqwest::blocking::Response>, String> {
        let since = Instant::now();
        let limit = Limit::Answer(s.first_within);
        match send_upstream(live, &s.route, method, kind, range, limit) {
            Err(e) => match self.recover(s, live, e.stalled, since) {
                Some(next) => {
                    *live = next;
                    send_upstream(live, &s.route, method, kind, range, limit).map_err(|e| e.text)
                }
                None => Err(e.text),
            },
            Ok(up) => Ok(up),
        }
    }

    /// What a request on `seen`'s client goes on with after it failed
    /// before its end: the session's upstream half to try it once more on,
    /// or `None` to give up. `stalled` is whether it timed out, and `since`
    /// when it last got anything.
    ///
    /// A connection that broke is tried again on the same client: over
    /// HTTP/1.1 the client makes a new connection with the same SOCKS
    /// credentials, so over Tor it is the same circuit and the same exit,
    /// and the stream's URLs (which only work from the address that
    /// resolved them) still work.
    ///
    /// One that stalled may be stuck on a circuit that stopped moving, and
    /// over HTTP/2 every later request would wait out the idle timeout on
    /// the same connection. So the client is replaced, but only if it is
    /// still the session's (requests that stalled together, video and audio,
    /// make one replacement between them: the first to get here makes it, and
    /// the others find it), nothing came over it since this request last got
    /// anything (then it is this request that is stuck, not the client), and
    /// none was replaced in the last `RECOVER_EVERY` (a route that keeps
    /// stalling ends its streams instead). On a route that isolates, a new
    /// client is a new identity, and the old URLs would get a 403 from its
    /// exit (linking a third identity to the two): so the video is looked up
    /// again instead (see `refresh`), and the new client shares that
    /// lookup's identity. On any other route the new client keeps the URLs.
    /// The lookup kept for playing the video again holds the old client, so
    /// it is dropped either way. `None` too when the session is over or the
    /// route has changed.
    pub(super) fn recover(
        &self,
        s: &Session,
        seen: &Live,
        stalled: bool,
        since: Instant,
    ) -> Option<Live> {
        if s.cancelled.load(Ordering::Acquire) || !self.runtime().net.same_route(&s.route) {
            return None;
        }
        let isolating = s.route.isolates();
        // A lookup again is one at a time, so what the others did is looked
        // at under its lock.
        let _one = (stalled && isolating).then(|| lock(&s.refreshing));
        let mut live = lock(&s.live);
        if live.generation != seen.generation || live.dial != seen.dial {
            return Some(live.clone());
        }
        if !stalled || seen.served.after(since) {
            return Some(live.clone());
        }
        if !s.may_recover() {
            return None;
        }
        if isolating {
            drop(live);
            return match self.resolve_again(s, seen.generation) {
                Ok(fresh) => Some(fresh),
                Err(e) => {
                    note!("qveil: couldn't look up a stalled video again: {e:#}");
                    None
                }
            };
        }
        live.client = s.route.pinned().relay_client().ok()?;
        live.served = Arc::default();
        live.pool = Arc::default();
        live.dial += 1;
        let fresh = live.clone();
        drop(live);
        self.lookups.forget(&s.source.id);
        Some(fresh)
    }

    /// Answers a range with one response while a second thread fetches it
    /// upstream chunk by chunk (see `CHUNK`). `first` is the first chunk's
    /// response, already checked by `relay`, and `start` what `relay` has
    /// read of its body.
    #[allow(clippy::too_many_arguments)]
    fn relay_chunked(
        self: &Arc<Self>,
        out: &mut TcpStream,
        s: &Arc<Session>,
        kind: &str,
        live: Live,
        first: reqwest::blocking::Response,
        start: Vec<u8>,
        plan: Plan,
    ) -> Result<()> {
        let ctype = up_type(&first).map(String::from);
        self.serve_ahead(
            out,
            s,
            kind,
            live,
            ctype,
            plan,
            First::Response(first, start),
        )
    }

    /// Answers a range that starts inside the head fetched at open: its
    /// bytes first, as they arrive, and then the rest, fetched upstream from
    /// where they end (or from where the head stopped, if it stopped short).
    #[allow(clippy::too_many_arguments)]
    fn relay_kept(
        self: &Arc<Self>,
        out: &mut TcpStream,
        s: &Arc<Session>,
        kind: &str,
        live: Live,
        w: Wanted,
        head: Arc<Head>,
        meta: Meta,
    ) -> Result<()> {
        let end = w.end.map_or(meta.total - 1, |e| e.min(meta.total - 1));
        let plan = Plan {
            start: w.start,
            first_end: end.min(meta.last),
            end,
            total: meta.total,
        };
        self.serve_ahead(
            out,
            s,
            kind,
            live,
            meta.content_type,
            plan,
            First::Kept(head),
        )
    }

    /// Sends the head of a 206 for `plan` and then what a second thread
    /// fetches: `plan.start..=plan.first_end` from `first`, and the bytes
    /// after that upstream.
    #[allow(clippy::too_many_arguments)]
    fn serve_ahead(
        self: &Arc<Self>,
        out: &mut TcpStream,
        s: &Arc<Session>,
        kind: &str,
        live: Live,
        ctype: Option<String>,
        plan: Plan,
        first: First,
    ) -> Result<()> {
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
        let player = Watch::new(out);
        ahead.drain_into(out, &s.cancelled, &player)
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
        self.resolve_again(s, seen)
    }

    /// `refresh`, once its lock is held and nobody else has. One that
    /// failed is not tried again for `seen` within `FAILED_FOR`: requests
    /// that got a 403 together would otherwise each run yt-dlp in turn,
    /// one after the other, to fail the same way. It is stopped (and isn't
    /// taken for a failure) once nobody waits for it.
    fn resolve_again(&self, s: &Session, seen: u32) -> Result<Live> {
        let rt = self.runtime();
        if !rt.net.same_route(&s.route) {
            // The user switched routes while this stream was playing. Resolving
            // it again would send a request over the route they left.
            bail!("The route changed; open the video again");
        }
        if let Some((generation, at, why)) = &*lock(&s.failed)
            && *generation == seen
            && at.elapsed() < FAILED_FOR
        {
            bail!("{why}");
        }
        let yt = rt.pinned_ytdlp();
        let players = RefCell::default();
        let gone = || s.nobody_waits(&players);
        let streams = match yt.resolve_until(&s.source.watch_url(), &s.format, &gone) {
            Ok(streams) => streams,
            Err(e) => {
                if !gone() {
                    *lock(&s.failed) = Some((seen, Instant::now(), format!("{e:#}")));
                }
                return Err(e);
            }
        };
        let fresh = Live {
            generation: seen + 1,
            dial: 0,
            video: streams.video,
            audio: streams.audio,
            headers: streams.headers,
            client: yt.net.relay_client()?,
            served: Arc::default(),
            pool: Arc::default(),
        };
        *lock(&s.live) = fresh.clone();
        s.warm.forget_older(fresh.generation);
        // A lookup kept for playing it again has stopped working too.
        self.lookups.forget(&s.source.id);
        Ok(fresh)
    }
}

/// Starts the upstream request for one relayed GET or HEAD, with `range` as
/// its Range header. `None` means there is no separate audio stream.
pub(super) fn fetch_upstream(
    live: &Live,
    route: &Net,
    method: &str,
    kind: &str,
    range: Option<&str>,
) -> Result<Option<reqwest::blocking::Response>, UpError> {
    send_upstream(live, route, method, kind, range, Limit::Idle)
}

/// How long an upstream request may take, besides the client's idle
/// timeout.
#[derive(Clone, Copy)]
enum Limit {
    /// No longer.
    Idle,
    /// All of it, connecting and the whole body included.
    All(Duration),
    /// Its answer, connecting included, has to start within this; the body
    /// then has the idle timeout.
    Answer(Duration),
}

/// `fetch_upstream`, given up on as `limit` says.
fn send_upstream(
    live: &Live,
    route: &Net,
    method: &str,
    kind: &str,
    range: Option<&str>,
    limit: Limit,
) -> Result<Option<reqwest::blocking::Response>, UpError> {
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
    if let Limit::All(t) = limit {
        rb = rb.timeout(t);
    }
    live.pool.take();
    let sent = match limit {
        Limit::Answer(within) => match answer_within(rb, within) {
            Some(sent) => sent,
            None => {
                return Err(UpError {
                    stalled: true,
                    text: format!(
                        "the stream's server didn't answer within {} s",
                        within.as_secs()
                    ),
                });
            }
        },
        _ => rb.send(),
    };
    match sent {
        Ok(up) => {
            live.served.mark();
            Ok(Some(up))
        }
        Err(e) => Err(UpError {
            stalled: e.is_timeout(),
            text: friendly(&route.explain(&format!("{:#}", e.without_url()))),
        }),
    }
}

/// Sends `rb`, and waits `within` for its answer to start: `None` if it
/// hasn't by then. (A request's own timeout would cover its whole body
/// too.) The request carries on until the client's idle timeout, on a
/// thread of its own, and its answer is dropped if it comes later.
fn answer_within(
    rb: reqwest::blocking::RequestBuilder,
    within: Duration,
) -> Option<reqwest::Result<reqwest::blocking::Response>> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    // If the thread can't be made, `rb` goes with it, and that is taken
    // for a stall too: tried again once.
    let _ = std::thread::Builder::new().spawn(move || {
        let _ = tx.send(rb.send());
    });
    rx.recv_timeout(within).ok()
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

    /// The Range header of the first upstream request: at most one chunk,
    /// and a small one unless it is the start of the stream. A range that
    /// starts anywhere else follows a seek, and the player may well seek
    /// again before it is through: a small chunk is in sooner, and keeps
    /// one that is given up on from holding the circuit for long.
    fn first_chunk(&self) -> String {
        let size = if self.start == 0 { CHUNK } else { SEEK_CHUNK };
        let last = self.start.saturating_add(size - 1);
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
    /// the player is left short of the Content-Length. So does a player
    /// that hangs up while nothing comes (`player` watches its socket):
    /// otherwise that would only be found out once the next bytes came,
    /// and the fetcher would carry on for it until then.
    fn drain_into(
        &self,
        out: &mut impl Write,
        cancelled: &AtomicBool,
        player: &Watch,
    ) -> Result<()> {
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
                        Some(Err(e)) => {
                            return Err(StreamBroke {
                                err: io::Error::other(e.clone()),
                                stalled_since: None,
                            }
                            .into());
                        }
                        None if player.gone() => return Ok(()),
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

/// Where a relayed range's first bytes come from.
enum First {
    /// The first chunk's response, already checked, and the start of its
    /// body that was read to check it.
    Response(reqwest::blocking::Response, Vec<u8>),
    /// The head fetched at open (see `warm`), as far as `chunk_end`.
    Kept(Arc<Head>),
}

/// How reading one chunk's body ended.
enum Taken {
    Complete,
    Stopped,
    /// Upstream ended it, or sent more than it was asked for.
    Broke(String),
    /// A read failed: it timed out (`stalled`), or the connection broke.
    /// `since` is when the last byte came.
    Failed {
        e: String,
        stalled: bool,
        since: Instant,
    },
}

impl Fetcher {
    fn run(mut self, first: First) {
        let result = self.fetch(first);
        lock(&self.ahead.state).done = Some(result);
        self.ahead.changed.notify_all();
    }

    fn stopped(&self) -> bool {
        self.session.cancelled.load(Ordering::Acquire) || lock(&self.ahead.state).gone
    }

    fn fetch(&mut self, first: First) -> Result<(), String> {
        // Where the last connection failed. A connection that drops part-way
        // is picked up again at the byte where it broke, but a second failure
        // at the same byte ends the stream.
        let mut failed_at = None;
        let mut up = match first {
            First::Response(up, start) => {
                self.pass_on(start);
                up
            }
            First::Kept(head) => {
                self.take_kept(&head);
                if self.stopped() || self.pos > self.end {
                    return Ok(());
                }
                match self.next(&mut failed_at)? {
                    Some(up) => up,
                    None => return Ok(()),
                }
            }
        };
        loop {
            match self.take(&mut up) {
                Taken::Stopped => return Ok(()),
                Taken::Complete if self.pos > self.end => return Ok(()),
                Taken::Complete => {}
                Taken::Broke(e) => once_at(self.pos, &mut failed_at, e)?,
                Taken::Failed { e, stalled, since } => {
                    self.recover(stalled, since, &mut failed_at, e)?;
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
                Err(Failure::Connection { e, stalled, since }) => {
                    self.recover(stalled, since, failed_at, e)?;
                }
                Err(Failure::Final(e)) => return Err(e),
            }
        }
    }

    /// Passes on what `head` has from `pos` up to `chunk_end`, as it
    /// arrives, until it has all been passed on or the head stops short
    /// (it failed, or stalled): the rest is then fetched upstream.
    fn take_kept(&mut self, head: &Head) {
        let mut block = Vec::new();
        loop {
            let stop = || self.stopped();
            let n = head.read_at(self.pos, self.chunk_end, 64 << 10, &mut block, &stop);
            if n == 0 {
                return;
            }
            self.pos += n as u64;
            let mut st = lock(&self.ahead.state);
            st.bytes += n;
            st.blocks.push_back(std::mem::take(&mut block));
            drop(st);
            self.ahead.changed.notify_all();
        }
    }

    /// Hands `block`, the next bytes of the current chunk, to `ahead`
    /// (never more than the chunk).
    fn pass_on(&mut self, mut block: Vec<u8>) {
        block.truncate(((self.chunk_end + 1).saturating_sub(self.pos)) as usize);
        if block.is_empty() {
            return;
        }
        self.pos += block.len() as u64;
        let mut st = lock(&self.ahead.state);
        st.bytes += block.len();
        st.blocks.push_back(block);
        drop(st);
        self.ahead.changed.notify_all();
    }

    /// Reads the current chunk's body into `ahead`.
    fn take(&mut self, up: &mut reqwest::blocking::Response) -> Taken {
        let mut buf = vec![0u8; 64 << 10];
        let mut since = Instant::now();
        loop {
            if self.stopped() {
                return Taken::Stopped;
            }
            let n = match up.read(&mut buf) {
                Ok(0) if self.pos > self.chunk_end => {
                    self.live.pool.put_back();
                    return Taken::Complete;
                }
                Ok(0) => return Taken::Broke("the stream ended early upstream".into()),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) if self.stopped() => return Taken::Stopped,
                Err(e) => {
                    return Taken::Failed {
                        stalled: stalled_read(&e),
                        e: e.to_string(),
                        since,
                    };
                }
            };
            since = self.live.served.mark();
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

    /// What the next request goes on with after the last one failed (see
    /// `Server::recover`), or the error that ends the stream: the second
    /// failure in a row at the same byte on the same client, or one that
    /// isn't to be recovered from. A new client (or a new lookup) gets an
    /// attempt of its own.
    fn recover(
        &mut self,
        stalled: bool,
        since: Instant,
        failed_at: &mut Option<u64>,
        e: String,
    ) -> Result<(), String> {
        let Some(next) = self
            .server
            .recover(&self.session, &self.live, stalled, since)
        else {
            return Err(e);
        };
        if next.generation != self.live.generation || next.dial != self.live.dial {
            *failed_at = None;
        }
        self.live = next;
        once_at(self.pos, failed_at, e)
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
        let get = |live: &Live| {
            let since = Instant::now();
            match fetch_upstream(live, route, "GET", &self.kind, Some(&range)) {
                Ok(Some(up)) => Ok(up),
                Ok(None) => Err(Failure::Final("no separate audio".into())),
                Err(e) => Err(Failure::Connection {
                    e: e.text,
                    stalled: e.stalled,
                    since,
                }),
            }
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
        if !up_type(&up).is_none_or(is_media_type) {
            return Err(Failure::Final(
                "upstream sent something other than the stream".into(),
            ));
        }
        match content_range(&up) {
            Some((a, b, total)) if a == self.pos && total == self.total && b <= self.end => {
                self.chunk_end = b;
                if self.server.sparing.load(Ordering::Relaxed) {
                    self.session
                        .keep_spare(&self.server, &self.live, &self.kind, &up);
                }
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
    /// The connection failed, or timed out (`stalled`) after `since`; worth
    /// one more try.
    Connection {
        e: String,
        stalled: bool,
        since: Instant,
    },
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
/// Each read that brings bytes is marked in `served`.
fn copy_body(
    up: &mut impl Read,
    out: &mut impl Write,
    cancelled: &AtomicBool,
    served: &Served,
) -> Result<()> {
    let mut buf = vec![0u8; 64 << 10];
    let mut since = Instant::now();
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let n = match up.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) if cancelled.load(Ordering::Acquire) => return Ok(()),
            Err(e) => {
                return Err(StreamBroke {
                    stalled_since: stalled_read(&e).then_some(since),
                    err: e,
                }
                .into());
            }
        };
        since = served.mark();
        // The read can have taken a long time.
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        if out.write_all(&buf[..n]).is_err() {
            return Ok(());
        }
    }
}

/// What `relay` answers in place of a stream that isn't one.
const NOT_A_STREAM: &[u8] =
    b"the stream is a playlist (HLS or DASH) or not a video at all, which qveil can't relay through your route";

/// How much of a stream's start `sniff` reads at most.
const SNIFF_MAX: usize = 4 << 10;

/// The Content-Type of a response, if it says one.
pub(super) fn up_type(up: &reqwest::blocking::Response) -> Option<&str> {
    up.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok())
}

/// HLS and DASH manifests, however the server spells the type.
fn is_playlist_type(content_type: &str) -> bool {
    let t = content_type.to_ascii_lowercase();
    t.contains("mpegurl") || t.contains("dash+xml")
}

/// A type a stream may be sent as: video, audio, bytes of no stated kind,
/// or none at all; not a manifest's.
pub(super) fn is_media_type(content_type: &str) -> bool {
    let t = content_type.split(';').next().unwrap_or("").trim();
    let t = t.to_ascii_lowercase();
    let media = t.is_empty()
        || t.starts_with("video/")
        || t.starts_with("audio/")
        || t == "application/octet-stream";
    media && !is_playlist_type(&t)
}

/// Whether a body that starts with `start` is an HLS or a DASH manifest,
/// after a byte order mark and white space, whatever it was sent as.
pub(super) fn is_manifest(start: &[u8]) -> bool {
    let s = start.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(start);
    let s = s.trim_ascii_start();
    [&b"#EXTM3U"[..], b"<?xml", b"<MPD"]
        .iter()
        .any(|m| s.get(..m.len()).is_some_and(|p| p.eq_ignore_ascii_case(m)))
}

/// Reads the start of a body, enough for `is_manifest` to tell: until 8
/// bytes past any byte order mark and white space are in, it ends, or
/// `SNIFF_MAX` are. Only the start of a stream is looked at: a range
/// from inside a video can hold any bytes, and mpv has read the start
/// (and so taken it for what it is) before it asks for one.
pub(super) fn sniff(up: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut got = vec![0u8; SNIFF_MAX];
    let mut n = 0;
    loop {
        let s = got[..n].strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&got[..n]);
        if s.trim_ascii_start().len() >= 8 || n == SNIFF_MAX {
            break;
        }
        match up.read(&mut got[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    got.truncate(n);
    Ok(got)
}

/// The upstream side of a relayed stream failed part-way through the body.
#[derive(Debug)]
pub(super) struct StreamBroke {
    err: io::Error,
    /// When it last got a byte, if it then timed out waiting for the next.
    stalled_since: Option<Instant>,
}

impl std::fmt::Display for StreamBroke {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the stream broke upstream: {}", self.err)
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
    use std::io::{Read, Write};
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
                served: Arc::default(),
                pool: Arc::default(),
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
    fn a_manifest_is_refused_whatever_type_it_is_sent_as() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        for path in [
            "/hls?type=video/mp4",
            "/hls?type=application/octet-stream&pad=1",
            "/hls?type=text/plain",
            "/dash?type=video/mp4",
            "/dash?type=audio/webm&xml=1&pad=1",
            "/dash?type=text/html",
            "/hls?type=audio/x-mpegurl",
        ] {
            let sid = add_session(&s, up.url(path));
            let stream = format!("s/{sid}/v");
            // Whole, from the start, and as the start of a long range.
            for range in [None, Some("bytes=0-99"), Some("bytes=0-")] {
                let headers: Vec<_> = range.iter().map(|r| ("Range", *r)).collect();
                let (status, _, body) = fetch(&base, "GET", &stream, &headers);
                let text = String::from_utf8_lossy(&body);
                assert_eq!(status, 502, "{path} {range:?}: {text}");
                assert!(text.contains("can't relay"), "{text}");
                assert!(!text.contains("googlevideo"), "{text}");
            }
        }
        // A video passes, whatever its type says: video, audio, plain bytes,
        // or nothing.
        let sid = add_session(&s, up.url("/video"));
        let (status, _, body) = fetch(&base, "GET", &format!("s/{sid}/v"), &[]);
        assert_eq!((status, body), (200, payload()));
        for t in [
            "video/mp4",
            "Audio/WebM; codecs=opus",
            "application/octet-stream",
            "",
        ] {
            assert!(is_media_type(t), "{t}");
        }
        for t in ["text/plain", "text/html; charset=utf-8", "application/json"] {
            assert!(!is_media_type(t), "{t}");
        }
        for start in [
            &b"#EXTM3U\n"[..],
            b"\xEF\xBB\xBF\r\n  #extm3u",
            b"<?xml version=\"1.0\"?><MPD>",
            b"\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\">",
        ] {
            assert!(is_manifest(start), "{start:?}");
        }
        assert!(!is_manifest(b"\0\0\0\x18ftypmp42"));
        assert!(!is_manifest(b"\x1a\x45\xdf\xa3"));
        assert!(!is_manifest(b""));
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
        copy_body(&mut up, &mut out, &cancel, &Served::default()).unwrap();
        // The second chunk arrived after the change, so it isn't passed on.
        assert_eq!((up.sent, out.as_slice()), (2, &b"data"[..]));

        // Already cancelled: not a single read.
        let mut out = Vec::new();
        copy_body(&mut up, &mut out, &cancel, &Served::default()).unwrap();
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

    /// A yt-dlp that answers like `fake_ytdlp` the first time, and runs
    /// `later` (shell) every time after that, with its run number in `$n`.
    fn ytdlp_then(dir: &std::path::Path, url: &str, later: &str) -> String {
        let script = r#"d=$(dirname "$0")
n=$(cat "$d/runs" 2>/dev/null || echo 0)
n=$((n + 1))
echo $n > "$d/runs.new" && mv "$d/runs.new" "$d/runs"
if [ $n -gt 1 ]; then
@LATER@
fi
cat <<EOF
{"title":"T","requested_formats":[
 {"url":"@URL@$n","protocol":"https","vcodec":"avc1"},
 {"url":"@URL@$n","protocol":"https","vcodec":"none"}]}
EOF
"#
        .replace("@LATER@", later)
        .replace("@URL@", url);
        crate::store::testing::script(&dir.join("yt-dlp"), &script)
    }

    #[test]
    fn requests_that_403_together_after_a_failed_resolve_share_its_failure() {
        let up = Upstream::start();
        let bin = TempDir::new();
        // The resolve after the 403 takes a moment and fails.
        let later = "sleep 1; echo 'ERROR: [youtube] boom' >&2; exit 1";
        let ytdlp = ytdlp_then(&bin.0, &up.url("/video?gen="), later);
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let info = s.open(&test_video()).unwrap();
        // Video and audio, and mpv asking for the video again.
        let streams = [
            info.video.clone(),
            info.audio.clone().unwrap(),
            info.video.clone(),
        ];
        let players: Vec<_> = streams
            .into_iter()
            .map(|url| {
                let base = base.clone();
                std::thread::spawn(move || fetch(&base, "GET", rel(&base, &url), &[]))
            })
            .collect();
        for p in players {
            let (status, _, body) = p.join().unwrap();
            assert_eq!(status, 502, "{}", String::from_utf8_lossy(&body));
        }
        // A request that comes a moment later gets it too.
        assert_eq!(fetch(&base, "GET", rel(&base, &info.video), &[]).0, 502);
        assert_eq!(runs(&bin.0), 2, "one resolve again between them");
    }

    /// The process id that `ytdlp_then`'s hanging run recorded, once it has.
    fn hung_pid(dir: &std::path::Path) -> String {
        let mut pid = String::new();
        wait_for("yt-dlp to hang", Duration::from_secs(10), || {
            pid = std::fs::read_to_string(dir.join("pid")).unwrap_or_default();
            pid.ends_with('\n')
        });
        pid.trim().to_string()
    }

    const HANG: &str = "echo $$ > \"$d/pid.new\" && mv \"$d/pid.new\" \"$d/pid\"; exec sleep 60";

    fn gone(pid: &str) {
        wait_for("yt-dlp to be stopped", Duration::from_secs(5), || {
            !std::path::Path::new("/proc").join(pid).exists()
        });
    }

    #[test]
    fn a_resolve_again_is_stopped_when_the_player_hangs_up() {
        let up = Upstream::start();
        let bin = TempDir::new();
        let ytdlp = ytdlp_then(&bin.0, &up.url("/video?gen="), HANG);
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let info = s.open(&test_video()).unwrap();
        let mut player = TcpStream::connect(addr(&base)).unwrap();
        write!(
            player,
            "GET {} HTTP/1.1\r\nHost: x\r\n\r\n",
            path(&base, rel(&base, &info.video))
        )
        .unwrap();
        let pid = hung_pid(&bin.0);
        drop(player);
        gone(&pid);
    }

    #[test]
    fn a_resolve_again_is_stopped_when_the_session_ends() {
        let up = Upstream::start();
        let bin = TempDir::new();
        let ytdlp = ytdlp_then(&bin.0, &up.url("/video?gen="), HANG);
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let info = s.open(&test_video()).unwrap();
        let mut player = TcpStream::connect(addr(&base)).unwrap();
        player
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            player,
            "GET {} HTTP/1.1\r\nHost: x\r\n\r\n",
            path(&base, rel(&base, &info.video))
        )
        .unwrap();
        let pid = hung_pid(&bin.0);
        let sid = rel(&base, &info.video)
            .split('/')
            .nth(1)
            .unwrap()
            .to_string();
        s.session(&sid).unwrap().cancel();
        gone(&pid);
        // Its player is let go, without a byte of the stream.
        let mut got = Vec::new();
        let _ = player.read_to_end(&mut got);
        assert!(!got.starts_with(b"HTTP/1.1 2"), "{got:?}");
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
            w(0, None).unwrap().first_chunk(),
            format!("bytes=0-{}", CHUNK - 1)
        );
        assert_eq!(
            w(7, None).unwrap().first_chunk(),
            format!("bytes=7-{}", 7 + SEEK_CHUNK - 1)
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

        // After a seek: from wherever the player asks, not from a boundary,
        // and a small chunk first.
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
                format!("bytes={from}-{}", from + SEEK_CHUNK - 1),
                format!("bytes={}-{}", from + SEEK_CHUNK, BIG - 1),
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
                format!("bytes=100-{}", 100 + SEEK_CHUNK - 1),
                format!(
                    "bytes={}-{}",
                    100 + SEEK_CHUNK,
                    100 + SEEK_CHUNK + CHUNK - 1
                ),
                format!("bytes={}-{last}", 100 + SEEK_CHUNK + CHUNK),
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
    fn a_player_that_hangs_up_while_nothing_comes_is_let_go_at_once() {
        let up = Upstream::start();
        let relay = Freezer::start(up.addr);
        let idle = Duration::from_secs(3);
        let (s, base, _d) = server_tuned(Config::default(), |rt| {
            rt.net = rt.net.clone().idling_after(idle);
            rt.yt.net = rt.net.clone();
        });
        let sid = add_session(&s, relay.url("/big?pace=5"));
        let session = s.session(&sid).unwrap();
        let mut player = start_ranged(&base, &sid);
        let mut buf = vec![0u8; 64 << 10];
        let mut got = 0;
        while got < 100 << 10 {
            got += player.read(&mut buf).unwrap();
        }
        // The route stalls, the player takes all there is, and hangs up
        // while the relay waits for more.
        relay.freeze();
        player
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        while player.read(&mut buf).is_ok_and(|n| n > 0) {}
        let dropped = Instant::now();
        drop(player);
        wait_for("the relay to let go", idle, || {
            lock(&session.clients).is_empty()
        });
        eprintln!("let go after {:?}", dropped.elapsed());
        // And when the stalled read gives up, nothing is asked for again
        // for a player that has gone.
        std::thread::sleep(idle + Duration::from_secs(1));
        assert_eq!(up.requests().len(), 1, "{:?}", up.requests());
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

    #[test]
    fn a_seek_finds_a_connection_ready_instead_of_dialling_one() {
        // Over Tor a new connection costs about 400 ms before its first
        // answer, and every answer 300 ms; then about 400 KB/s.
        let dial = Duration::from_millis(400);
        let up = KeepAlive::start(dial);
        let (s, base, _d) = server();
        s.sparing.store(true, std::sync::atomic::Ordering::Relaxed);
        let size = 40u64 << 20;
        let sid = add_session(&s, up.url(&format!("/big?size={size}&ttfb=300&pace=40")));
        let mut player = start_ranged(&base, &sid);
        let mut buf = vec![0u8; 64 << 10];
        let mut got = 0;
        while got < 100 << 10 {
            got += player.read(&mut buf).unwrap();
        }
        // It plays a while, and then the owner seeks past what is cached.
        std::thread::sleep(Duration::from_millis(1500));
        drop(player);
        let from = 20u64 << 20;
        let (head, took, _, _seek) =
            first_byte(&base, &format!("s/{sid}/v"), &format!("bytes={from}-"));
        eprintln!("first byte after the seek: {took:?}");
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert!(took < Duration::from_millis(600), "{took:?}");
        // The spare was made while the first range played, and the range
        // after the seek, which is small, went over it.
        let asked = up.ranges();
        assert_eq!(asked[1], "bytes=0-0", "{asked:?}");
        assert!(
            asked.contains(&format!("bytes={from}-{}", from + SEEK_CHUNK - 1)),
            "{asked:?}"
        );
        assert!(up.connections() >= 2);
    }

    /// Over HTTP/1.1, a session keeps one connection to spare at most, and
    /// asks for it rarely: the one-byte request it takes is a request
    /// googlevideo sees, and a spare dialled while the others are busy only
    /// adds a connection.
    #[test]
    fn over_many_chunks_and_seeks_a_spare_is_asked_for_rarely_and_connections_stay_few() {
        let up = KeepAlive::start(Duration::from_millis(30));
        let (s, base, _d) = server();
        s.sparing.store(true, std::sync::atomic::Ordering::Relaxed);
        let size = 40u64 << 20;
        let sid = add_session(&s, up.url(&format!("/big?size={size}")));
        let stream = format!("s/{sid}/v");
        let started = Instant::now();
        // It plays a moment from the start, and the owner seeks past what
        // is cached: the player hangs up mid-chunk.
        let mut player = start_ranged(&base, &sid);
        let mut buf = vec![0u8; 64 << 10];
        let mut got = 0;
        while got < 100 << 10 {
            got += player.read(&mut buf).unwrap();
        }
        wait_for("the spare", Duration::from_secs(5), || {
            up.ranges().iter().any(|r| r == "bytes=0-0")
        });
        std::thread::sleep(Duration::from_millis(100));
        drop(player);
        // Then seeks here and there, each played through more than one
        // chunk, and a long stretch of full chunks.
        let mut asked: Vec<(u64, u64)> = (0..12u64)
            .map(|i| {
                let from = (i * (3 << 20) + 12_345) % (size - (2 << 20));
                (from, from + (3 << 19))
            })
            .collect();
        asked.push((0, (25 << 20) - 1));
        for (from, to) in asked {
            std::thread::sleep(Duration::from_millis(30));
            let (status, _, body) = fetch(
                &base,
                "GET",
                &stream,
                &[("Range", &format!("bytes={from}-{to}"))],
            );
            assert_eq!(status, 206);
            assert!(body == big_bytes(from, to + 1), "bytes {from}-{to}");
        }
        let took = started.elapsed();
        let pings = up.ranges().iter().filter(|r| *r == "bytes=0-0").count();
        eprintln!(
            "{pings} pings, {} connections in {took:?}",
            up.connections()
        );
        assert!(
            pings as u64 <= 1 + took.as_secs() / 10,
            "{pings} pings in {took:?}: {:?}",
            up.ranges()
        );
        assert!(up.connections() <= 3, "{} connections", up.connections());
    }

    #[test]
    fn no_spare_is_asked_for_over_http2() {
        use crate::h2up::H2Upstream;
        let up = H2Upstream::start();
        let (s, base, _d) = server_h2c();
        s.sparing.store(true, std::sync::atomic::Ordering::Relaxed);
        let sid = add_session(&s, up.url("/big"));
        for (from, to) in [(0u64, CHUNK + 99), (5 << 20, (5 << 20) + 99)] {
            let (status, _, _) = fetch(
                &base,
                "GET",
                &format!("s/{sid}/v"),
                &[("Range", &format!("bytes={from}-{to}"))],
            );
            assert_eq!(status, 206);
        }
        std::thread::sleep(Duration::from_millis(200));
        let ranges = up.ranges();
        assert!(
            !ranges.iter().any(|r| r.as_deref() == Some("bytes=0-0")),
            "{ranges:?}"
        );
        assert_eq!(up.connections(), 1);
    }

    /// A spare that isn't ready within `SPARE_WITHIN` is no use for the
    /// seek it is for (over Tor one made while the circuit was busy took
    /// 11 s): it is given up on, and doesn't hold the next one back.
    #[test]
    fn a_spare_that_is_slow_to_connect_is_given_up_on() {
        // The first connection is quick, and every one after it hangs.
        let up = KeepAlive::start_dialling(|n| Duration::from_secs(if n == 0 { 0 } else { 20 }));
        let (s, base, _d) = server();
        s.sparing.store(true, std::sync::atomic::Ordering::Relaxed);
        let size = 40u64 << 20;
        let sid = add_session(&s, up.url(&format!("/big?size={size}&pace=20")));
        let session = s.session(&sid).unwrap();
        let mut player = start_ranged(&base, &sid);
        let mut buf = vec![0u8; 64 << 10];
        assert!(player.read(&mut buf).unwrap() > 0);
        wait_for("the spare to be dialled", Duration::from_secs(5), || {
            up.connections() == 2
        });
        let dialled = Instant::now();
        assert!(lock(&session.spare).dialing);
        wait_for(
            "the spare to be given up on",
            Duration::from_secs(5),
            || !lock(&session.spare).dialing,
        );
        let took = dialled.elapsed();
        assert!(
            took >= SPARE_WITHIN - Duration::from_millis(500)
                && took < SPARE_WITHIN + Duration::from_secs(1),
            "{took:?}"
        );
        drop(player);
    }

    #[test]
    fn a_request_whose_answer_does_not_start_is_given_up_on_well_before_the_idle_timeout() {
        // Every connection is taken and then never answered (for 30 s).
        let up = KeepAlive::start(Duration::from_secs(30));
        let (s, base, _d) = server();
        let rt = s.runtime();
        let mut session = session_for(&rt, up.url("/big"));
        session.first_within = Duration::from_millis(500);
        let sid = s.register(&rt, session).unwrap();
        let started = Instant::now();
        let (status, _, body) = fetch(
            &base,
            "GET",
            &format!("s/{sid}/v"),
            &[("Range", "bytes=0-")],
        );
        let took = started.elapsed();
        assert_eq!(status, 502, "{}", String::from_utf8_lossy(&body));
        assert!(!String::from_utf8_lossy(&body).contains("127.0.0.1"));
        // Once on the client, and once on the new one put in its place.
        assert_eq!(up.connections(), 2);
        assert!(took < Duration::from_secs(10), "{took:?}");
    }

    /// A session on `up` with sparing on, whose spare runs the hook that
    /// `make` makes (given the server's base URL) right before it would go
    /// out, and a player reading from its start.
    fn spare_with_hook<H: Fn() + Send + Sync + 'static>(
        make: impl FnOnce(String) -> H,
    ) -> (KeepAlive, Arc<Server>, TempDir, Arc<Session>, TcpStream) {
        let up = KeepAlive::start(Duration::ZERO);
        let (s, base, d) = server();
        s.sparing.store(true, Ordering::Relaxed);
        let sid = add_session(&s, up.url(&format!("/big?size={}&pace=20", 40u64 << 20)));
        let session = s.session(&sid).unwrap();
        lock(&session.spare).before_send = Some(Arc::new(make(base.clone())));
        let mut player = start_ranged(&base, &sid);
        let mut buf = vec![0u8; 64 << 10];
        assert!(player.read(&mut buf).unwrap() > 0);
        (up, s, d, session, player)
    }

    #[test]
    fn a_spare_is_not_asked_for_on_a_route_the_user_has_just_left() {
        let switched = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&switched);
        let (up, _s, _d, session, _player) = spare_with_hook(move |base| {
            move || {
                // The owner switches to Tor while the spare is on its way out.
                switch_to_tor(&base);
                done.store(true, Ordering::SeqCst);
            }
        });
        wait_for("the spare to be done", Duration::from_secs(10), || {
            switched.load(Ordering::SeqCst) && !lock(&session.spare).dialing
        });
        let asked = up.ranges();
        assert!(!asked.iter().any(|r| r == "bytes=0-0"), "{asked:?}");
    }

    #[test]
    fn a_spare_that_fails_badly_does_not_hold_the_next_one_back() {
        let (_up, _s, _d, session, _player) =
            spare_with_hook(|_| || panic!("a spare that broke (as this test wants)"));
        wait_for("the spare to be let go", Duration::from_secs(5), || {
            !lock(&session.spare).dialing
        });
    }

    #[test]
    fn the_first_chunk_after_a_seek_is_small_and_the_next_is_full_size() {
        let up = Upstream::start();
        let (s, base, _d) = server();
        let size = 40u64 << 20;
        let sid = add_session(&s, up.url(&format!("/big?size={size}")));
        let from = 20u64 << 20;
        let (_, _, _, _seek) = first_byte(&base, &format!("s/{sid}/v"), &format!("bytes={from}-"));
        wait_for("the second chunk", Duration::from_secs(5), || {
            ranges(&up).len() >= 2
        });
        let next = from + SEEK_CHUNK;
        assert_eq!(
            ranges(&up)[..2],
            [
                format!("bytes={from}-{}", next - 1),
                format!("bytes={next}-{}", next + CHUNK - 1)
            ]
        );
        // From the start, it is a full chunk at once.
        let (_, _, _, _start) = first_byte(&base, &format!("s/{sid}/v"), "bytes=0-");
        wait_for("the first chunk", Duration::from_secs(5), || {
            ranges(&up).len() >= 3
        });
        assert!(ranges(&up).contains(&format!("bytes=0-{}", CHUNK - 1)));
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
        let first = s.open(&test_video()).unwrap();
        let (status, _, _) = fetch(
            &base,
            "GET",
            rel(&base, &first.video),
            &[("Range", "bytes=0-99")],
        );
        assert_eq!(status, 206);
        // Played again from the kept lookup, on its client, whose connection
        // has stalled meanwhile: the warm-up of the new session waits it out.
        // (Nothing else is in flight on the client: bytes that came for
        // another request after the warm-up asked would say it isn't stuck.)
        relay.freeze();
        s.warming.store(true, std::sync::atomic::Ordering::Relaxed);
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

    /// The size of the video behind a `Gate`.
    const GATED: u64 = 6 << 20;

    /// A server on an isolating route through `gate`, whose relay clients
    /// give up on a connection that has said nothing for `IDLE`, and whose
    /// yt-dlp resolves both streams to `/big` on it, paced `pace` ms per
    /// 16 KiB, for the identity it is given.
    fn behind_a_gate(gate: &Gate, bin: &TempDir, pace: u64) -> (Arc<Server>, String, TempDir) {
        let ytdlp = gate_ytdlp(
            &bin.0,
            &format!("http://gv.test/big?size={GATED}&pace={pace}"),
        );
        server_tuned(
            Config {
                ytdlp,
                proxy: Some(gate.proxy()),
                isolate_circuits: true,
                ..Config::default()
            },
            |rt| {
                rt.net = rt.net.clone().idling_after(IDLE);
                rt.yt.net = rt.net.clone();
            },
        )
    }

    /// Plays `want` bytes of the video from the start, on a thread.
    fn play_from_start(base: &str, video: &str, want: u64) -> std::thread::JoinHandle<Fetched> {
        let (base, path) = (base.to_string(), rel(base, video).to_string());
        std::thread::spawn(move || {
            fetch(
                &base,
                "GET",
                &path,
                &[("Range", &format!("bytes=0-{}", want - 1))],
            )
        })
    }

    fn sent(gate: &Gate) -> usize {
        gate.gauge.sent.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn a_connection_reset_part_way_is_tried_again_on_the_same_identity() {
        let gate = Gate::start();
        let bin = TempDir::new();
        let (s, base, _d) = behind_a_gate(&gate, &bin, 5);
        let info = s.open(&test_video()).unwrap();
        let want = 2u64 << 20;
        let player = play_from_start(&base, &info.video, want);
        wait_for("bytes to flow", Duration::from_secs(5), || {
            sent(&gate) > 256 << 10
        });
        gate.reset();
        let (status, _, body) = player.join().unwrap();
        assert_eq!(status, 206);
        assert!(body == big_bytes(0, want), "{} bytes", body.len());
        // A new connection on the same client: the same identity, so the
        // same circuit, and the URL still works. Nothing was looked up again.
        assert_eq!(gate.identities().len(), 1, "{:?}", gate.requests());
        assert_eq!(gate.refused(), 0, "{:?}", gate.requests());
        assert_eq!(runs(&bin.0), 1);
    }

    #[test]
    fn a_stall_on_an_isolating_route_looks_the_video_up_again_on_one_new_identity() {
        let gate = Gate::start();
        let bin = TempDir::new();
        let (s, base, _d) = behind_a_gate(&gate, &bin, 5);
        let info = s.open(&test_video()).unwrap();
        let want = 2u64 << 20;
        let player = play_from_start(&base, &info.video, want);
        wait_for("bytes to flow", Duration::from_secs(5), || {
            sent(&gate) > 256 << 10
        });
        gate.freeze();
        let (status, _, body) = player.join().unwrap();
        assert_eq!(status, 206);
        assert!(body == big_bytes(0, want), "{} bytes", body.len());
        // The old URL was never tried on a new identity (that is a sure 403
        // and a third identity): the video was looked up again on the one
        // the rest of it then came over.
        assert_eq!(runs(&bin.0), 2);
        assert_eq!(gate.identities().len(), 2, "{:?}", gate.requests());
        assert_eq!(gate.refused(), 0, "{:?}", gate.requests());
    }

    #[test]
    fn stalls_that_keep_coming_end_the_stream_instead_of_looking_it_up_again_and_again() {
        let gate = Gate::start();
        let bin = TempDir::new();
        // Slow enough that the second stall comes well before the end, under load too.
        let (s, base, _d) = behind_a_gate(&gate, &bin, 20);
        let info = s.open(&test_video()).unwrap();
        let want = 5u64 << 20;
        let player = play_from_start(&base, &info.video, want);
        wait_for("bytes to flow", Duration::from_secs(5), || {
            sent(&gate) > 256 << 10
        });
        gate.freeze();
        wait_for("the second lookup", Duration::from_secs(5), || {
            runs(&bin.0) == 2
        });
        let before = sent(&gate);
        wait_for("bytes to flow again", Duration::from_secs(5), || {
            sent(&gate) > before + (256 << 10)
        });
        gate.freeze();
        let (status, _, body) = player.join().unwrap();
        assert_eq!(status, 206);
        assert!((body.len() as u64) < want, "{} bytes", body.len());
        assert!(body == big_bytes(0, body.len() as u64), "the bytes differ");
        assert_eq!(runs(&bin.0), 2);
        assert_eq!(gate.identities().len(), 2, "{:?}", gate.requests());
        assert_eq!(gate.refused(), 0, "{:?}", gate.requests());
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
