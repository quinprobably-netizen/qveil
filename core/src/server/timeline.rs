//! A live stream's timeline: what the player is told exists, and the
//! session that fetches it.
//!
//! A running stream's segments are numbered, and the playlist lists the ones
//! the engine knows exist (`oldest..=head`). It is an event playlist, so the
//! player rewinds over the whole of it and keeps asking for the rest until
//! `#EXT-X-ENDLIST` says it is over. A stream that is only a list of
//! fragments plays as a finished video.
//!
//! The playlists themselves are pure text (`playlist`, `listed_playlist`).
//! `LiveSession` is what serves them: it learns the head from upstream,
//! fetches each segment the player asks for over the route, three more ahead
//! of it, and finds out when the stream ends.

use super::Server;
use super::http::respond;
use super::relay::{FAILED_FOR, RECOVER_EVERY, answer_within, content_range, stalled_read};
use crate::errors::{self, friendly};
use crate::ids::VideoRef;
use crate::live::{self, Fragments, LiveStreams, Opening, Track};
use crate::net::Net;
use crate::util::lock;
use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, RANGE};
use reqwest::{Method, StatusCode};
use serde_json::json;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Whether the timeline says what it does on stderr, one `qveil-trace` line
/// per event: each request upstream, each segment served, each recovery.
/// Only with `QVEIL_LIVE_TRACE=1`, for measuring. No URL is ever in it.
fn tracing() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("QVEIL_LIVE_TRACE").is_some_and(|v| v == "1"))
}

macro_rules! trace {
    ($($arg:tt)*) => {
        if tracing() {
            let at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
            note!("qveil-trace {:.3} {}", at.as_secs_f64(), format_args!($($arg)*));
        }
    };
}

/// Milliseconds since `t`, for the trace.
fn ms(t: Instant) -> u128 {
    t.elapsed().as_millis()
}

/// How far back a running stream can be rewound: five days.
pub(super) const WINDOW_SECS: f64 = 432_000.0;

/// Segments kept back from the window's edge, so the oldest one listed has
/// not just been dropped upstream by the time the player asks for it.
pub(super) const BACK_MARGIN: u64 = 60;

/// Which segments of a running stream exist.
pub(super) struct Span {
    /// The oldest segment listed.
    pub oldest: u64,
    /// The newest segment that exists, inclusive.
    pub head: u64,
    /// The stream is over: the playlist ends.
    pub ended: bool,
}

/// The oldest segment a playlist lists: the rewind window back from `head`,
/// plus the margin, and never past `head`. A stream younger than the window
/// (less the margin) lists everything from its first segment, 0.
pub(super) fn oldest_for(head: u64, target: f64) -> u64 {
    let window = (WINDOW_SECS / target) as u64;
    head.saturating_add(BACK_MARGIN)
        .saturating_sub(window)
        .min(head)
}

/// The playlist of a running (or just ended) stream's track: segment `n` is
/// `{prefix}{n}.{ext}`, each `target` seconds long.
pub(super) fn playlist(span: &Span, target: f64, prefix: &str, ext: &str) -> String {
    let mut out = header("EVENT", target_duration(target), span.oldest);
    for n in span.oldest..=span.head {
        entry(&mut out, target, &format!("{prefix}{n}.{ext}"));
    }
    if span.ended {
        out.push_str("#EXT-X-ENDLIST\n");
    }
    out
}

/// The playlist of a track that is a list of fragments: a finished video,
/// whose segment `i` (counting from 0) is `durations[i]` seconds long.
pub(super) fn listed_playlist(durations: &[f64], prefix: &str, ext: &str) -> String {
    let longest = durations.iter().copied().fold(0.0, f64::max);
    let mut out = header("VOD", target_duration(longest), 0);
    for (i, duration) in durations.iter().enumerate() {
        entry(&mut out, *duration, &format!("{prefix}{i}.{ext}"));
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}

/// A playlist's target duration: whole seconds, rounded up, at least 1.
fn target_duration(secs: f64) -> u64 {
    secs.ceil().max(1.0) as u64
}

fn header(kind: &str, target: u64, first: u64) -> String {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target}\n\
         #EXT-X-PLAYLIST-TYPE:{kind}\n#EXT-X-MEDIA-SEQUENCE:{first}\n"
    )
}

fn entry(out: &mut String, duration: f64, uri: &str) {
    // Writing to a String cannot fail.
    let _ = write!(out, "#EXTINF:{duration:.3},\n{uri}\n");
}

/// Segments of a track fetched ahead of the one the player asks for. One
/// 1440p segment takes longer over Tor than it plays, and several fetched
/// side by side all keep up (measured, spec appendix 2).
const AHEAD: u64 = 3;
/// Segments held per track: the one asked for and those ahead of it.
const HELD: usize = 4;
/// How close to the head a segment that upstream doesn't have is taken to
/// be not there yet, rather than dropped off the back of the window.
const EDGE: u64 = 2 * AHEAD;
/// Nobody has asked for anything for this many segment lengths: nobody is
/// playing, and the head is not polled. A player at the edge asks for the
/// playlist once per segment length; one that is paused, or rewound with
/// its cache full, stops asking, and the first thing it asks for again
/// starts the polls again (a stale head costs it one reload). A window that
/// closed the video without playing another (which would end the session)
/// is gone within this.
const IDLE_SEGMENTS: f64 = 2.0;
/// The head standing still for this many segment lengths is when a check
/// lookup asks whether the stream has ended. Each check that says it has
/// not doubles the wait for the next, up to `CHECK_AT_MOST`.
const STILL_SEGMENTS: u32 = 3;
const CHECK_AT_MOST: Duration = Duration::from_secs(300);
/// The largest segment that is read: 4K at 12 Mbps is 7.5 MB per 5 s.
const SEGMENT_MAX: u64 = 64 << 20;
/// How often a request that waits looks at the cancel flag.
const WAIT_POLL: Duration = Duration::from_millis(250);
/// How long a request may go without a byte coming in before it is taken to
/// have stalled: its answer not starting, or a segment's body stopping part
/// way. It is then made again on a fresh client (see `recover`), a segment
/// from the byte it stopped at. The player gets the bytes as they come, so
/// its own network timeout (60 s) counts from the last of them, and a stall
/// with its redo fits well inside it: the player sees a slow segment, never
/// a skipped one.
const STALL: Duration = Duration::from_secs(10);
/// A request made again this many times in a row without a byte more coming
/// in is given up on: on a route that isolates, a stall is made again on a
/// fresh client of the same identity, and a second stall in a row on a
/// fresh identity (see `recover`); the third is the end of it. Three
/// stalls and one lookup take about 45 s over Tor, within the player's
/// own network timeout (60 s), so that it sees a slow segment, not a gap.
const STRIKES: u32 = 3;
/// The most requests one fetch makes, however much each brings.
const TRIES: u32 = 8;
/// How long the join's segments, fetched before the player asks for them
/// (see `begin`), wait for it to: it asks within seconds, unless the
/// window has gone somewhere else meanwhile.
const WARM_FOR: Duration = Duration::from_secs(60);
/// How much of a body is read, or written to the player, at a time.
const CHUNK: usize = 64 << 10;

/// What a lookup again says of a stream that is no live stream any more.
const NOT_LIVE: &str = "This video is no longer a live stream; open it again.";

/// Whether a check lookup that failed with `err` says positively that the
/// stream is over: it ended and YouTube kept no recording, or it ended and
/// nothing of it can be fetched (`live::ENDED`). Nothing else does: not a
/// bot check or a limit on how fast (YouTube words that one "Video
/// unavailable" too), not a running stream's answer without fragments (an
/// HLS-only one, which another identity may well get), not a timeout.
fn ends_it(err: &str) -> bool {
    let code = errors::explain(err).code;
    !matches!(code, errors::Code::RateLimited | errors::Code::BotCheck)
        && (code == errors::Code::NoRecording || err.contains(live::ENDED))
}

/// What the engine's own words end with when the head's response said nothing of it.
const NO_HEAD: &str = "The stream's server didn't say how far the stream has got.";

/// The two tracks of a live session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kind {
    Video,
    Audio,
}

impl Kind {
    /// The track a segment's path names (`v` or `a`).
    pub(super) fn named(name: &str) -> Option<Kind> {
        match name {
            "v" => Some(Kind::Video),
            "a" => Some(Kind::Audio),
            _ => None,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// `v` or `a`, as in a segment's path.
    fn letter(self) -> &'static str {
        match self {
            Kind::Video => "v",
            Kind::Audio => "a",
        }
    }

    /// Where a playlist's segments are, next to it.
    fn prefix(self) -> &'static str {
        match self {
            Kind::Video => "v/",
            Kind::Audio => "a/",
        }
    }
}

/// The upstream half of a live session: where its fragments are, and the
/// client of the identity that looked them up (the URLs only work from it).
#[derive(Clone)]
struct Upstream {
    /// Counts lookups, so that requests refused together share one.
    generation: u32,
    /// Counts the clients put in place of a stalled one since the last
    /// lookup, so that requests that stalled together make one between them.
    dial: u32,
    video: Track,
    audio: Track,
    headers: Vec<(String, String)>,
    /// The identity the URLs were looked up on: on a route that isolates,
    /// the only one they work from. A fresh client is made on it.
    ident: Net,
    client: Client,
}

impl Upstream {
    fn track(&self, kind: Kind) -> &Track {
        match kind {
            Kind::Video => &self.video,
            Kind::Audio => &self.audio,
        }
    }
}

/// Why a segment couldn't be had.
#[derive(Clone, Debug)]
enum Failure {
    /// It is not there any more (or a listed one never was): 410.
    Gone,
    /// The session ended while it was being fetched.
    Cancelled,
    /// Upstream failed, in words for the player (without the URL: it is
    /// signed for this address).
    Broke(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Gone => f.write_str("This part of the stream is no longer offered."),
            Failure::Cancelled => f.write_str("stream expired"),
            Failure::Broke(why) => f.write_str(why),
        }
    }
}

/// A segment being fetched, or fetched: the bytes in so far, which each
/// request serving it passes on to the player as they come, and kept once
/// they are all in (a player that asks again gets them from here).
struct Fetch {
    got: Mutex<Got>,
    /// Told when bytes come in, and when it is done.
    grew: Condvar,
    /// It is no longer held (a seek dropped it, or the player read past
    /// it): it stops being fetched once no request is serving it.
    dropped: AtomicBool,
    /// The requests serving it now (see `Reading`).
    readers: AtomicUsize,
}

#[derive(Default)]
struct Got {
    bytes: Vec<u8>,
    /// How long the whole segment is, once upstream has said.
    length: Option<u64>,
    /// Set once it is done: all of it in, or why not.
    done: Option<Result<(), Failure>>,
}

impl Fetch {
    fn new() -> Self {
        Fetch {
            got: Mutex::default(),
            grew: Condvar::new(),
            dropped: AtomicBool::new(false),
            readers: AtomicUsize::new(0),
        }
    }

    fn failed(why: Failure) -> Self {
        let f = Fetch::new();
        f.finish(Err(why));
        f
    }

    fn len(&self) -> u64 {
        lock(&self.got).bytes.len() as u64
    }

    fn length(&self) -> Option<u64> {
        lock(&self.got).length
    }

    /// Whether it failed upstream, and is to be fetched again if asked for.
    fn broke(&self) -> bool {
        matches!(lock(&self.got).done, Some(Err(Failure::Broke(_))))
    }

    /// Upstream says the segment is `length` long: false if an earlier
    /// answer said otherwise (the bytes in so far are then of another cut).
    fn set_length(&self, length: u64) -> bool {
        let mut got = lock(&self.got);
        match got.length {
            Some(known) => known == length,
            None => {
                got.length = Some(length);
                let more = usize::try_from(length)
                    .unwrap_or(0)
                    .saturating_sub(got.bytes.len());
                got.bytes.reserve(more);
                self.grew.notify_all();
                true
            }
        }
    }

    fn push(&self, bytes: &[u8]) {
        lock(&self.got).bytes.extend_from_slice(bytes);
        self.grew.notify_all();
    }

    fn finish(&self, how: Result<(), Failure>) {
        let mut got = lock(&self.got);
        if got.done.is_none() {
            if how.is_ok() && got.length.is_none() {
                got.length = Some(got.bytes.len() as u64);
            }
            got.done = Some(how);
        }
        self.grew.notify_all();
    }

    fn drop_it(&self) {
        self.dropped.store(true, Ordering::Release);
    }

    /// Dropped, and nobody is reading it: fetching the rest is waste.
    fn unwanted(&self) -> bool {
        self.dropped.load(Ordering::Acquire) && self.readers.load(Ordering::Acquire) == 0
    }
}

/// A request serving a fetch, for as long as it does (see `Fetch::readers`).
struct Reading(Arc<Fetch>);

impl Reading {
    fn new(fetch: &Arc<Fetch>) -> Self {
        fetch.readers.fetch_add(1, Ordering::AcqRel);
        Reading(Arc::clone(fetch))
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A track's segments held: the one asked for last and those ahead of it,
/// in order.
type Held = VecDeque<(u64, Arc<Fetch>)>;

/// What a request failed with before it was done.
struct Broken {
    text: String,
    /// It timed out: the connection stalled, rather than broke.
    stalled: bool,
    /// Making it again can't help (an answer that doesn't fit the bytes in
    /// so far, or one too large).
    fatal: bool,
}

impl Broken {
    fn broke(text: impl Into<String>) -> Self {
        Broken {
            text: text.into(),
            stalled: false,
            fatal: false,
        }
    }

    fn fatal(text: impl Into<String>) -> Self {
        Broken {
            fatal: true,
            ..Broken::broke(text)
        }
    }
}

/// What a request was answered with: its status and the head it said. The
/// body of a segment's is in its fetch.
struct Answer {
    status: StatusCode,
    head: Option<u64>,
}

/// When things last happened, for the head's watcher.
struct Clock {
    /// A fragment last came in (it says where the head is).
    fetched: Option<Instant>,
    /// The head was last asked for.
    polled: Instant,
    /// The head last moved.
    moved: Instant,
    /// The player last asked for anything.
    asked: Instant,
    /// Requests waiting for a segment ahead of the head.
    ahead: usize,
}

/// A live stream being played: two playlists and their segments, fetched
/// over the route the stream was opened on.
pub(super) struct LiveSession {
    source: VideoRef,
    /// The cap the tracks were chosen with, to look the stream up again the same way.
    max_height: u32,
    /// The route it was opened on, not one pinned identity of it: a fresh
    /// identity is derived from it each time the stream is looked up again.
    pub(super) route: Net,
    upstream: Mutex<Upstream>,
    /// Held while looking up again, so simultaneous failures share one lookup.
    refreshing: Mutex<()>,
    /// The last lookup again that failed: the generation, when, and why.
    failed: Mutex<Option<(u32, Instant, String)>>,
    /// When a stalled client was last replaced.
    recovered: Mutex<Option<Instant>>,
    span: Mutex<Span>,
    /// Told when the head moves, a segment comes in, or the session ends.
    changed: Condvar,
    held: [Mutex<Held>; 2],
    /// The join's segments, fetched before the player asks (see `begin`
    /// and `take_warm`), and until when they wait for it.
    warm: [Mutex<Held>; 2],
    warm_until: Mutex<Option<Instant>>,
    clock: Mutex<Clock>,
    /// Set when the session ends: evicted, the route changed, or another
    /// video played. Every thread of it stops before its next request.
    pub(super) cancelled: AtomicBool,
    fragments: Fragments,
    /// Each track's segment extension.
    exts: [&'static str; 2],
    /// How long a segment is, in seconds.
    target: f64,
    /// The stream had ended when it was looked up.
    ended_at_open: bool,
    /// `STALL` in milliseconds, which tests shorten.
    pub(super) stall_ms: AtomicU64,
}

impl LiveSession {
    /// A session for the stream `s`, looked up on `ident`, a pinned
    /// identity of `route`, which its requests go out on.
    pub(super) fn new(
        source: VideoRef,
        max_height: u32,
        route: Net,
        s: &LiveStreams,
        ident: Net,
    ) -> Result<Self> {
        let now = Instant::now();
        let client = ident.relay_client()?;
        Ok(LiveSession {
            source,
            max_height,
            route,
            upstream: Mutex::new(Upstream {
                generation: 0,
                dial: 0,
                video: s.video.clone(),
                audio: s.audio.clone(),
                headers: s.headers.clone(),
                ident,
                client,
            }),
            refreshing: Mutex::new(()),
            failed: Mutex::new(None),
            recovered: Mutex::new(None),
            span: Mutex::new(Span {
                oldest: 0,
                head: 0,
                ended: false,
            }),
            changed: Condvar::new(),
            held: Default::default(),
            warm: Default::default(),
            warm_until: Mutex::new(None),
            clock: Mutex::new(Clock {
                fetched: None,
                polled: now,
                moved: now,
                asked: now,
                ahead: 0,
            }),
            cancelled: AtomicBool::new(false),
            fragments: s.fragments.clone(),
            exts: [s.video.ext, s.audio.ext],
            target: s.target,
            ended_at_open: s.ended,
            stall_ms: AtomicU64::new(STALL.as_millis() as u64),
        })
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn segment(&self) -> Duration {
        Duration::from_secs_f64(self.target)
    }

    fn stall(&self) -> Duration {
        Duration::from_millis(self.stall_ms.load(Ordering::Relaxed))
    }

    /// Ends the session: every thread of it stops before its next request,
    /// and a request waiting on it is let go.
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify();
    }

    fn notify(&self) {
        let _span = lock(&self.span);
        self.changed.notify_all();
    }

    /// Learns where the stream is before the player is told of it: the head,
    /// from the final answer to a HEAD of a track's base (googlevideo may
    /// redirect it, and the host it ends on is the one that knows), and
    /// whether the oldest segment the window allows is still there. If it
    /// isn't (the streamer turned rewinding off), the playlist starts three
    /// segments back. Returns whether it can be rewound. A stream that is
    /// only a list of fragments has nothing to learn.
    ///
    /// Both tracks' bases are asked at once, and the first answer with the
    /// head is taken (the other track's host is reached meanwhile, for the
    /// requests that follow). As soon as the head is known, the segments
    /// the player joins at (the last `HELD` of each track) start coming in,
    /// while the rest of this, the answer and the player's start take
    /// their time: its first requests find them on their way (see
    /// `take_warm`).
    pub(super) fn begin(self: &Arc<Self>, server: &Arc<Server>) -> Result<bool> {
        if !matches!(self.fragments, Fragments::Numbered) {
            return Ok(true);
        }
        let began = Instant::now();
        let head = self.first_head(server)?;
        trace!("begin head={head} after_ms={}", ms(began));
        lock(&self.span).head = head;
        // (Off only in the tests that count every request upstream.)
        if !self.ended_at_open && server.warming.load(Ordering::Relaxed) {
            self.warm_join(server, head);
        }
        let mut oldest = oldest_for(head, self.target);
        if oldest + 3 < head
            && let Ok(a) = self.request(server, Method::HEAD, Kind::Audio, Some(oldest), None)
            && matches!(a.status.as_u16(), 404 | 410)
        {
            oldest = head - 3;
        }
        trace!("begin oldest={oldest} total_ms={}", ms(began));
        let mut span = lock(&self.span);
        span.oldest = oldest;
        span.head = span.head.max(head);
        span.ended = self.ended_at_open;
        Ok(oldest + 3 < span.head)
    }

    /// The head, from whichever track's base answers with it first (see
    /// `begin`). The other's request goes on by itself, and what it says
    /// of the head is taken too. An error only if neither can say.
    fn first_head(self: &Arc<Self>, server: &Arc<Server>) -> Result<u64> {
        let (tx, rx) = std::sync::mpsc::channel();
        for kind in [Kind::Video, Kind::Audio] {
            let (s, server, sender) = (Arc::clone(self), Arc::clone(server), tx.clone());
            let spawned = std::thread::Builder::new().spawn(move || {
                let said = match s.request(&server, Method::HEAD, kind, None, None) {
                    Ok(a) if a.status.is_success() => a.head.context(NO_HEAD),
                    Ok(a) => Err(anyhow::anyhow!(
                        "The stream's server answered {}.",
                        a.status.as_u16()
                    )),
                    Err(f) => Err(anyhow::anyhow!("{f}")),
                };
                let _ = sender.send((kind, said));
            });
            if let Err(e) = spawned {
                let _ = tx.send((kind, Err(e).context("too busy to ask the stream's server")));
            }
        }
        drop(tx);
        let mut failed = None;
        for (kind, said) in rx {
            match said {
                Ok(head) => return Ok(head),
                // The video's reason is the one given, if both fail.
                Err(e) if kind == Kind::Video || failed.is_none() => failed = Some(e),
                Err(_) => {}
            }
        }
        Err(failed.unwrap_or_else(|| anyhow::anyhow!(NO_HEAD)))
    }

    /// Starts fetching the segments a player joins at, `head - 3..=head` of
    /// both tracks, before it asks (see `begin`).
    fn warm_join(self: &Arc<Self>, server: &Arc<Server>, head: u64) {
        *lock(&self.warm_until) = Some(Instant::now() + WARM_FOR);
        for kind in [Kind::Video, Kind::Audio] {
            let mut warm = lock(&self.warm[kind.index()]);
            for n in head.saturating_sub(HELD as u64 - 1)..=head {
                let fetch = Arc::new(Fetch::new());
                warm.push_back((n, Arc::clone(&fetch)));
                self.fetch_in_background(server, kind, n, fetch);
            }
            trace!(
                "warm {} {:?}",
                kind.letter(),
                warm.iter().map(|(n, _)| *n).collect::<Vec<_>>()
            );
        }
    }

    /// Starts watching the head of a running stream (see `watch_head`).
    pub(super) fn start(self: &Arc<Self>, server: &Arc<Server>) {
        if !matches!(self.fragments, Fragments::Numbered) || lock(&self.span).ended {
            return;
        }
        let (s, server) = (Arc::clone(self), Arc::clone(server));
        let _ = std::thread::Builder::new().spawn(move || s.watch_head(&server));
    }

    /// Keeps the head known while the player sits at the edge and fetches
    /// nothing: a HEAD of the base about once per segment length while no
    /// fragment came in during the last one, and more often while a request
    /// waits for a segment ahead of it. Once the head has stood still for
    /// `STILL_SEGMENTS`, one check lookup asks whether the stream is over,
    /// and if it says so the playlists end. Nothing is asked while nobody
    /// plays, and it stops for good when the session ends.
    fn watch_head(&self, server: &Server) {
        let segment = self.segment();
        let tick = (segment / 4).max(Duration::from_millis(10));
        let idle = segment.mul_f64(IDLE_SEGMENTS);
        let still = segment * STILL_SEGMENTS;
        let mut check_in = still;
        let mut checked: Option<Instant> = None;
        let mut moved = lock(&self.clock).moved;
        loop {
            {
                let span = lock(&self.span);
                if self.is_cancelled() || span.ended {
                    return;
                }
                drop(self.changed.wait_timeout(span, tick));
            }
            if self.is_cancelled() || !server.runtime().net.same_route(&self.route) {
                return;
            }
            let poll = {
                let mut c = lock(&self.clock);
                if c.asked.elapsed() >= idle {
                    continue;
                }
                let quiet = c.fetched.is_none_or(|t| t.elapsed() >= segment);
                let every = if c.ahead > 0 { tick } else { segment };
                let poll = (quiet || c.ahead > 0) && c.polled.elapsed() >= every;
                if poll {
                    c.polled = Instant::now();
                }
                poll
            };
            if poll {
                // A failure is the next poll's to put right.
                let _ = self.request(server, Method::HEAD, Kind::Video, None, None);
            }
            // Judged on the head as this poll left it: after a while of
            // nobody playing, the head is old until the first poll.
            let check = {
                let c = lock(&self.clock);
                if c.moved != moved {
                    (moved, check_in, checked) = (c.moved, still, None);
                }
                c.moved.elapsed() >= check_in && checked.is_none_or(|t| t.elapsed() >= check_in)
            };
            if check {
                let ended = match self.check_end(server) {
                    _ if self.is_cancelled() => return,
                    Ok(ended) => ended,
                    // A stream that ended without a recording, or with
                    // nothing left that can be fetched, is never "ended" to
                    // a lookup: the lookup fails, saying so, and that is the
                    // end. The URLs the session has stay; they serve what
                    // is left.
                    Err(e) if ends_it(&format!("{e:#}")) => true,
                    Err(e) => {
                        note!("qveil: couldn't check whether a live stream has ended: {e:#}");
                        false
                    }
                };
                // The last head, should the polls before have missed it, and
                // the last word: a head that moved again is a stream that
                // goes on, whatever the lookup said, and is watched on.
                if ended && self.still_after_one_more_poll(server) {
                    lock(&self.span).ended = true;
                    self.notify();
                    return;
                }
                checked = Some(Instant::now());
                check_in = (check_in * 2).min(CHECK_AT_MOST.max(still));
            }
        }
    }

    /// Polls the head once more: whether it still hasn't moved. One that
    /// can't be asked for hasn't been seen to move.
    fn still_after_one_more_poll(&self, server: &Server) -> bool {
        let moved = lock(&self.clock).moved;
        let _ = self.request(server, Method::HEAD, Kind::Video, None, None);
        lock(&self.clock).moved == moved
    }

    /// One lookup that says whether the stream has ended. Its URLs and
    /// identity take the place of the session's, as any lookup's do.
    fn check_end(&self, server: &Server) -> Result<bool> {
        let _one = lock(&self.refreshing);
        let seen = lock(&self.upstream).generation;
        Ok(self.relook(server, seen)?.1)
    }

    /// `GET live/<sid>/video.m3u8` (or `audio.m3u8`).
    pub(super) fn serve_playlist(&self, out: &mut TcpStream, kind: Kind) -> Result<()> {
        if self.is_cancelled() {
            return respond(out, 404, "text/plain", b"stream expired");
        }
        self.asked();
        let ext = self.exts[kind.index()];
        let text = match &self.fragments {
            Fragments::Numbered => playlist(&lock(&self.span), self.target, kind.prefix(), ext),
            Fragments::Listed(list) => {
                let durations: Vec<f64> = list.iter().map(|(_, d)| *d).collect();
                listed_playlist(&durations, kind.prefix(), ext)
            }
        };
        respond(out, 200, "application/vnd.apple.mpegurl", text.as_bytes())
    }

    /// `GET live/<sid>/state`: where the stream is, for the window, which
    /// can't tell from the player when it has ended:
    /// `{"state":"now"|"ended","head":<n>,"oldest":<n>}`, the newest and
    /// oldest segments listed. Asking it doesn't count as playing: it keeps
    /// no head polls going (the player's own reloads do that).
    pub(super) fn serve_state(&self, out: &mut TcpStream) -> Result<()> {
        if self.is_cancelled() {
            return respond(out, 404, "text/plain", b"stream expired");
        }
        let (oldest, head, ended) = match &self.fragments {
            Fragments::Numbered => {
                let span = lock(&self.span);
                (span.oldest, span.head, span.ended)
            }
            Fragments::Listed(list) => (0, list.len().saturating_sub(1) as u64, true),
        };
        let state = json!({
            "state": if ended { "ended" } else { "now" },
            "head": head,
            "oldest": oldest,
        });
        respond(out, 200, "application/json", state.to_string().as_bytes())
    }

    /// `GET live/<sid>/v/<n>.<ext>` (or `a/`): segment `n`'s bytes, as
    /// upstream sent them. One behind the window is 410, one ahead of the
    /// head is waited for (two segment lengths at most), and asking for it
    /// starts the next `AHEAD` fetching.
    pub(super) fn serve_segment(
        self: &Arc<Self>,
        server: &Arc<Server>,
        out: &mut TcpStream,
        kind: Kind,
        name: &str,
    ) -> Result<()> {
        let ext = self.exts[kind.index()];
        let Some(n) = name
            .strip_suffix(ext)
            .and_then(|n| n.strip_suffix('.'))
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<u64>().ok())
        else {
            return respond(out, 404, "text/plain", b"no such segment");
        };
        if self.is_cancelled() {
            return respond(out, 404, "text/plain", b"stream expired");
        }
        self.asked();
        match self.exists(n) {
            Some(true) => {}
            Some(false) => {
                return respond(out, 410, "text/plain", Failure::Gone.to_string().as_bytes());
            }
            None => return respond(out, 404, "text/plain", b"not there yet"),
        }
        let asked = Instant::now();
        let reading = self.slot(server, kind, n);
        let fetch = &reading.0;
        // The answer starts once upstream's has said how long the segment
        // is (or, if it never says, once it is all in), and the bytes follow
        // as they come.
        let length = match self.when(fetch, |g| g.length.is_some() || g.done.is_some()) {
            Ok(g) => match (&g.done, g.length) {
                (Some(Err(f)), _) => Err(f.clone()),
                (_, Some(length)) => Ok(length),
                (_, None) => Err(Failure::Broke("The stream broke upstream.".into())),
            },
            Err(f) => Err(f),
        };
        let length = match length {
            Ok(length) => length,
            Err(Failure::Gone) => {
                self.gone(n);
                return respond(out, 410, "text/plain", Failure::Gone.to_string().as_bytes());
            }
            Err(Failure::Cancelled) => return respond(out, 404, "text/plain", b"stream expired"),
            Err(f @ Failure::Broke(_)) => {
                return respond(out, 502, "text/plain", f.to_string().as_bytes());
            }
        };
        let head = write!(
            out,
            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {length}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n",
            content_type(ext),
        );
        if let Err(e) = head {
            self.hung_up(kind, fetch);
            return Err(e.into());
        }
        trace!(
            "serve {} n={n} len={length} head_ms={}",
            kind.letter(),
            ms(asked)
        );
        let mut sent = 0;
        let how = loop {
            if sent as u64 >= length {
                break "done";
            }
            // A fetch that fails part way, or a session that ends, leaves
            // the answer short: the player knows it from the length.
            let Ok(g) = self.when(fetch, |g| g.bytes.len() > sent || g.done.is_some()) else {
                break "cancelled";
            };
            if g.bytes.len() <= sent {
                break "short";
            }
            let part = g.bytes[sent..g.bytes.len().min(sent + 4 * CHUNK)].to_vec();
            drop(g);
            if out.write_all(&part).is_err() {
                break "hung-up";
            }
            sent += part.len();
        };
        trace!(
            "served {} n={n} len={length} sent={sent} ms={} end={how}",
            kind.letter(),
            ms(asked)
        );
        if how == "hung-up" {
            self.hung_up(kind, fetch);
            bail!("the player hung up");
        }
        Ok(())
    }

    /// Waits until what is in of `fetch` is `ready`, or it is done: what is
    /// in, still locked. `Cancelled` once the session ends.
    fn when<'a>(
        &self,
        fetch: &'a Fetch,
        ready: impl Fn(&Got) -> bool,
    ) -> Result<MutexGuard<'a, Got>, Failure> {
        let mut got = lock(&fetch.got);
        loop {
            if ready(&got) {
                return Ok(got);
            }
            if self.is_cancelled() {
                return Err(Failure::Cancelled);
            }
            got = match fetch.grew.wait_timeout(got, WAIT_POLL) {
                Ok((got, _)) => got,
                Err(e) => e.into_inner().0,
            };
        }
    }

    fn asked(&self) {
        lock(&self.clock).asked = Instant::now();
    }

    /// Whether segment `n` exists (`Some(true)`), is gone (`Some(false)`),
    /// or isn't there, even after waiting up to two segment lengths for the
    /// head to reach it (`None`).
    fn exists(&self, n: u64) -> Option<bool> {
        let Fragments::Numbered = self.fragments else {
            return (usize::try_from(n).is_ok_and(|n| n < self.listed())).then_some(true);
        };
        let mut span = lock(&self.span);
        if n < span.oldest {
            return Some(false);
        }
        if n <= span.head {
            return Some(true);
        }
        if span.ended {
            return None;
        }
        let until = Instant::now() + self.segment() * 2;
        lock(&self.clock).ahead += 1;
        while n > span.head && !self.is_cancelled() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            span = match self.changed.wait_timeout(span, left.min(WAIT_POLL)) {
                Ok((span, _)) => span,
                Err(e) => e.into_inner().0,
            };
        }
        lock(&self.clock).ahead -= 1;
        (n <= span.head && !self.is_cancelled()).then_some(true)
    }

    fn listed(&self) -> usize {
        match &self.fragments {
            Fragments::Listed(list) => list.len(),
            Fragments::Numbered => 0,
        }
    }

    /// Segment `n` is gone upstream: the playlist starts after it from now
    /// on. Not near the head, where missing means not there yet.
    fn gone(&self, n: u64) {
        let mut span = lock(&self.span);
        if matches!(self.fragments, Fragments::Numbered) && n + EDGE < span.head {
            span.oldest = span.oldest.max(n + 1);
        }
    }

    /// The fetch of `kind`'s segment `n`, started if it isn't held, with
    /// the next `AHEAD` (those that exist) started beside it, and the caller
    /// counted as reading it. A request that isn't for one of the held
    /// segments, or the one after them, is a seek, and what was held goes.
    /// A fetch that failed is started again. One that goes stops being
    /// fetched once nobody reads it: a seek, or many, leaves no more than
    /// `HELD` fetches of a track running.
    fn slot(self: &Arc<Self>, server: &Arc<Server>, kind: Kind, n: u64) -> Reading {
        let last = match &self.fragments {
            Fragments::Numbered => lock(&self.span).head,
            Fragments::Listed(list) => list.len().saturating_sub(1) as u64,
        };
        let until = n.saturating_add(AHEAD).min(last).max(n);
        let mut held = lock(&self.held[kind.index()]);
        self.take_warm(kind, n, &mut held);
        let reading_on = held
            .front()
            .is_some_and(|(first, _)| (*first..=first + HELD as u64).contains(&n));
        let before = held.len();
        held.retain(|(m, f)| {
            let keep = reading_on && *m >= n && !f.broke();
            if !keep {
                f.drop_it();
            }
            keep
        });
        let dropped = before - held.len();
        let mut started = Vec::new();
        for m in n..=until {
            if held.iter().all(|(k, _)| *k != m) {
                let fetch = Arc::new(Fetch::new());
                held.push_back((m, Arc::clone(&fetch)));
                self.fetch_in_background(server, kind, m, fetch);
                started.push(m);
            }
        }
        trace!(
            "slot {} n={n} seek={} dropped={dropped} started={started:?}",
            kind.letter(),
            !reading_on && before > 0
        );
        held.make_contiguous().sort_by_key(|(m, _)| *m);
        while held.len() > HELD {
            if let Some((_, f)) = held.pop_back() {
                f.drop_it();
            }
        }
        let fetch = held
            .iter()
            .find(|(m, _)| *m == n)
            .map(|(_, f)| Arc::clone(f))
            .unwrap_or_else(|| Arc::new(Fetch::failed(Failure::Broke("not held".into()))));
        Reading::new(&fetch)
    }

    /// The join's segments (see `warm_join`) become what is held once the
    /// player asks for one of them: those from it on, which it reads on
    /// from, in place of whatever was held. A request for any other segment
    /// leaves them be: FFmpeg reads the first segments of a playlist when it
    /// opens it, and only then goes to where it was told to start. They go
    /// once the player asks for one past them, or after `WARM_FOR`.
    fn take_warm(&self, kind: Kind, n: u64, held: &mut Held) {
        let mut warm = lock(&self.warm[kind.index()]);
        let Some(&(last, _)) = warm.back() else {
            return;
        };
        if warm.iter().any(|(m, _)| *m == n) {
            for (_, f) in held.drain(..) {
                f.drop_it();
            }
            for (m, f) in warm.drain(..) {
                if m >= n && !f.broke() {
                    held.push_back((m, f));
                } else {
                    f.drop_it();
                }
            }
            trace!("took warm {} n={n}", kind.letter());
        } else if n > last || lock(&self.warm_until).is_none_or(|t| Instant::now() >= t) {
            for (_, f) in warm.drain(..) {
                f.drop_it();
            }
            trace!("dropped warm {} n={n}", kind.letter());
        }
    }

    /// The player hung up on `fetch` before it had all of it (FFmpeg does,
    /// on a seek): unless another request is serving it, it is no longer
    /// held, and stops being fetched at once instead of coming in whole for
    /// nobody. Asked for again, it is fetched anew.
    fn hung_up(&self, kind: Kind, fetch: &Arc<Fetch>) {
        if fetch.readers.load(Ordering::Acquire) > 1 || lock(&fetch.got).done.is_some() {
            return;
        }
        lock(&self.held[kind.index()]).retain(|(_, f)| !Arc::ptr_eq(f, fetch));
        lock(&self.warm[kind.index()]).retain(|(_, f)| !Arc::ptr_eq(f, fetch));
        fetch.drop_it();
    }

    fn fetch_in_background(
        self: &Arc<Self>,
        server: &Arc<Server>,
        kind: Kind,
        n: u64,
        fetch: Arc<Fetch>,
    ) {
        let (s, server, into) = (Arc::clone(self), Arc::clone(server), Arc::clone(&fetch));
        let spawned = std::thread::Builder::new().spawn(move || {
            let t = Instant::now();
            let how = s.fetch_segment(&server, kind, n, &into);
            trace!(
                "fetched {} n={n} bytes={} ms={} how={}",
                kind.letter(),
                into.len(),
                ms(t),
                match &how {
                    Ok(()) => "ok".to_string(),
                    Err(f) => format!("{f:?}").chars().take(60).collect(),
                }
            );
            into.finish(how);
        });
        if spawned.is_err() {
            fetch.finish(Err(Failure::Broke("too busy to fetch the stream".into())));
        }
    }

    /// Segment `n` of `kind`, into `into`. Missing near the head is asked
    /// for again once, half a segment length later; missing anywhere else,
    /// it is gone.
    fn fetch_segment(
        &self,
        server: &Server,
        kind: Kind,
        n: u64,
        into: &Fetch,
    ) -> Result<(), Failure> {
        let mut again = true;
        loop {
            let a = self.request(server, Method::GET, kind, Some(n), Some(into))?;
            if a.status.is_success() {
                return Ok(());
            }
            if !matches!(a.status.as_u16(), 404 | 410) {
                return Err(Failure::Broke(format!(
                    "The stream's server answered {}.",
                    a.status.as_u16()
                )));
            }
            let behind = match &self.fragments {
                Fragments::Numbered => n + EDGE < lock(&self.span).head,
                Fragments::Listed(_) => true,
            };
            if behind {
                return Err(Failure::Gone);
            }
            if !again {
                return Err(Failure::Broke(
                    "The stream's server doesn't have this part yet.".into(),
                ));
            }
            again = false;
            self.pause(self.segment() / 2);
        }
    }

    /// Sleeps for `d`, or until the session ends.
    fn pause(&self, d: Duration) {
        let until = Instant::now() + d;
        let mut span = lock(&self.span);
        while !self.is_cancelled() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            span = match self.changed.wait_timeout(span, left) {
                Ok((span, _)) => span,
                Err(e) => e.into_inner().0,
            };
        }
    }

    /// One request for `kind`'s segment `n`, its body read into `into`, or
    /// with no `n` a HEAD of its base (which says where the head is). One
    /// that stalls (no byte for `stall`) or breaks is made again on a fresh
    /// client (see `recover`), a segment's from the byte it had got to, so
    /// that what is in stays and nothing comes twice: the player is better
    /// off with a slow segment than with one skipped, which is a gap. Made
    /// again `STRIKES` times in a row without a byte more, it is given up
    /// on. A 403 means the URLs have stopped working, and the stream is
    /// looked up again (once, shared by the requests refused together).
    /// Nothing goes out once the session has ended, or once nobody wants
    /// the segment any more.
    fn request(
        &self,
        server: &Server,
        method: Method,
        kind: Kind,
        n: Option<u64>,
        into: Option<&Fetch>,
    ) -> Result<Answer, Failure> {
        let mut up = lock(&self.upstream).clone();
        let mut relooked = false;
        let (mut strikes, mut tries, mut stalls) = (0, 0, 0);
        loop {
            if self.is_cancelled() || into.is_some_and(Fetch::unwanted) {
                return Err(Failure::Cancelled);
            }
            let url = self.url(&up, kind, n).ok_or(Failure::Gone)?;
            let from = into.map_or(0, Fetch::len);
            let sent_at = Instant::now();
            let answer = self.send(&up, &method, &url, from);
            trace!(
                "up {method} {} n={n:?} from={from} gen={} dial={} -> {} redirected={} ms={}",
                kind.letter(),
                up.generation,
                up.dial,
                match &answer {
                    Ok(r) => r.status().as_u16().to_string(),
                    Err(b) if b.stalled => "stalled".to_string(),
                    Err(_) => "broke".to_string(),
                },
                answer.as_ref().is_ok_and(|r| r.url().as_str() != url),
                ms(sent_at)
            );
            let failed = match answer {
                Ok(r) if r.status() == StatusCode::FORBIDDEN && !relooked => {
                    relooked = true;
                    drop(r);
                    up = self
                        .refresh(server, up.generation)
                        .map_err(|e| Failure::Broke(friendly(&format!("{e:#}"))))?;
                    continue;
                }
                Ok(r) => {
                    let status = r.status();
                    let head = head_of(r.headers());
                    if status.is_success()
                        && let Some(h) = head
                    {
                        self.saw_head(h);
                    }
                    match into {
                        Some(into) if status.is_success() => match self.read_into(r, into, from) {
                            Ok(()) => {
                                lock(&self.clock).fetched = Some(Instant::now());
                                return Ok(Answer { status, head });
                            }
                            Err(e) => e,
                        },
                        _ => return Ok(Answer { status, head }),
                    }
                }
                Err(e) => e,
            };
            if self.is_cancelled() || into.is_some_and(Fetch::unwanted) {
                return Err(Failure::Cancelled);
            }
            tries += 1;
            if into.is_some_and(|f| f.len() > from) {
                (strikes, stalls) = (0, 0);
            } else {
                strikes += 1;
            }
            if failed.stalled {
                stalls += 1;
            }
            if failed.fatal || strikes >= STRIKES || tries >= TRIES {
                return Err(Failure::Broke(failed.text));
            }
            trace!(
                "failed {method} {} n={n:?} stalled={} strikes={strikes} stalls={stalls} tries={tries}",
                kind.letter(),
                failed.stalled
            );
            up = self
                .recover(server, &up, failed.stalled.then_some(stalls))
                .ok_or(Failure::Broke(failed.text))?;
        }
    }

    /// Reads the body of `r`, an answer to a request for a segment from
    /// byte `from` on, into `into`, as it comes. Stops when it is all in,
    /// when no byte has come for `stall` (it stalled), when the session
    /// ends or nobody wants it, or when it doesn't fit the bytes in so far.
    /// An answer that starts before `from` (a server that ignored the
    /// range) has what is in already skipped.
    fn read_into(&self, r: Response, into: &Fetch, from: u64) -> Result<(), Broken> {
        const CHANGED: &str = "The stream's server sent another version of this part.";
        let (start, length) = if r.status() == StatusCode::PARTIAL_CONTENT {
            let (start, _, total) = content_range(&r).ok_or(Broken::fatal(CHANGED))?;
            (start, Some(total))
        } else {
            (0, r.content_length())
        };
        if start > from {
            return Err(Broken::fatal(CHANGED));
        }
        if let Some(length) = length {
            if length > SEGMENT_MAX {
                return Err(Broken::fatal("This part of the stream is too large."));
            }
            if !into.set_length(length) {
                return Err(Broken::fatal(CHANGED));
            }
        }
        let mut skip = usize::try_from(from - start).unwrap_or(usize::MAX);
        // Read on a thread of its own, so that a read that never returns is
        // noticed here and left behind (the client's idle timeout ends it).
        // Once this stops taking what it reads, it hangs up.
        let (tx, rx) = sync_channel::<std::io::Result<Vec<u8>>>(4);
        let _ = std::thread::Builder::new().spawn(move || {
            let mut r = r.take(SEGMENT_MAX);
            loop {
                let mut buf = vec![0; CHUNK];
                let read = match r.read(&mut buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    read => read,
                };
                let last = !matches!(read, Ok(k) if k > 0);
                if tx
                    .send(read.map(|k| {
                        buf.truncate(k);
                        buf
                    }))
                    .is_err()
                    || last
                {
                    return;
                }
            }
        });
        let stall = self.stall();
        let mut quiet = Instant::now();
        loop {
            if self.is_cancelled() || into.unwanted() {
                return Err(Broken::broke("stream expired"));
            }
            match rx.recv_timeout(WAIT_POLL.min(stall)) {
                Ok(Ok(bytes)) if bytes.is_empty() => {
                    return match into.length() {
                        Some(length) if into.len() < length => {
                            Err(Broken::broke("The stream broke upstream."))
                        }
                        _ => Ok(()),
                    };
                }
                Ok(Ok(bytes)) => {
                    quiet = Instant::now();
                    let k = skip.min(bytes.len());
                    skip -= k;
                    let mut bytes = &bytes[k..];
                    let room = into
                        .length()
                        .unwrap_or(SEGMENT_MAX)
                        .saturating_sub(into.len());
                    if bytes.len() as u64 > room {
                        bytes = &bytes[..usize::try_from(room).unwrap_or(0)];
                    }
                    into.push(bytes);
                }
                Ok(Err(e)) => {
                    return Err(Broken {
                        stalled: stalled_read(&e),
                        ..Broken::broke("The stream broke upstream.")
                    });
                }
                Err(RecvTimeoutError::Timeout) if quiet.elapsed() >= stall => {
                    return Err(Broken {
                        stalled: true,
                        ..Broken::broke(format!(
                            "The stream's server stopped sending for {} s.",
                            stall.as_secs()
                        ))
                    });
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Broken::broke("The stream broke upstream."));
                }
            }
        }
    }

    fn url(&self, up: &Upstream, kind: Kind, n: Option<u64>) -> Option<String> {
        let base = &up.track(kind).base;
        match (n, &self.fragments) {
            (None, _) => Some(base.clone()),
            (Some(n), Fragments::Numbered) => Some(format!("{base}&sq={n}")),
            (Some(n), Fragments::Listed(list)) => list
                .get(usize::try_from(n).ok()?)
                .map(|(path, _)| format!("{base}{path}")),
        }
    }

    /// Sends one request, for the bytes from `from` on if that isn't 0, and
    /// waits `stall` for its answer to start.
    fn send(
        &self,
        up: &Upstream,
        method: &Method,
        url: &str,
        from: u64,
    ) -> Result<Response, Broken> {
        let mut rb = up.client.request(method.clone(), url);
        for (k, v) in &up.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if from > 0 {
            rb = rb.header(RANGE, format!("bytes={from}-"));
        }
        let within = self.stall();
        match answer_within(rb, within) {
            None => Err(Broken {
                stalled: true,
                ..Broken::broke(format!(
                    "The stream's server didn't answer within {} s.",
                    within.as_secs()
                ))
            }),
            Some(Ok(r)) => Ok(r),
            Some(Err(e)) => Err(Broken {
                stalled: e.is_timeout(),
                ..Broken::broke(friendly(
                    &self.route.explain(&format!("{:#}", e.without_url())),
                ))
            }),
        }
    }

    /// A fragment or the base said the head is at `head`.
    fn saw_head(&self, head: u64) {
        if !matches!(self.fragments, Fragments::Numbered) {
            return;
        }
        {
            let mut span = lock(&self.span);
            if head <= span.head || span.ended {
                return;
            }
            span.head = head;
            lock(&self.clock).moved = Instant::now();
            self.changed.notify_all();
        }
        // A player joins three segments back from the head it last saw:
        // once that is past the join's segments fetched ahead (FFmpeg's
        // reads at open can take a while), they are of no use, and only
        // take from the circuit what the player waits for.
        for kind in [Kind::Video, Kind::Audio] {
            let mut warm = lock(&self.warm[kind.index()]);
            if warm.back().is_some_and(|(last, _)| head > last + 2) {
                for (_, f) in warm.drain(..) {
                    f.drop_it();
                }
                trace!("stale warm {} head={head}", kind.letter());
            }
        }
    }

    /// What a request on `seen` goes on with after it failed before its
    /// end: `None` to give up. One that broke is made again on the same
    /// client (a new connection, the same identity). One that stalled
    /// (`stalls` is how many times in a row it has, without a byte more)
    /// may be stuck on a connection or a circuit that stopped moving, so the
    /// client is replaced, if it is still the session's (requests that
    /// stalled together make one replacement): by a fresh client on the
    /// same identity, which the URLs still work from, so that a segment
    /// resumes from the byte it had got to, of the same cut. Tor gives the
    /// new connection the same circuit if that one still works, and a new
    /// one if it doesn't. On a route that isolates, a second stall in a row
    /// looks the stream up again instead, on a fresh identity (at most once
    /// every `RECOVER_EVERY`; within that, the request is made again on the
    /// client there is), as `Server::recover` does; a 403 (see `request`)
    /// does at once. `None` too once the session is over or the route has
    /// changed.
    fn recover(&self, server: &Server, seen: &Upstream, stalls: Option<u32>) -> Option<Upstream> {
        if self.is_cancelled() || !server.runtime().net.same_route(&self.route) {
            return None;
        }
        let isolating = self.route.isolates();
        let relook = isolating && stalls.is_some_and(|k| k >= 2);
        let _one = relook.then(|| lock(&self.refreshing));
        let mut up = lock(&self.upstream);
        if up.generation != seen.generation || up.dial != seen.dial || stalls.is_none() {
            return Some(up.clone());
        }
        trace!("recover stalls={stalls:?} relook={relook}");
        if relook {
            if !self.may_recover() {
                return Some(up.clone());
            }
            drop(up);
            return match self.relook(server, seen.generation) {
                Ok((fresh, _)) => Some(fresh),
                Err(e) => {
                    note!("qveil: couldn't look up a stalled live stream again: {e:#}");
                    None
                }
            };
        }
        up.client = up.ident.relay_client().ok()?;
        up.dial += 1;
        Some(up.clone())
    }

    fn may_recover(&self) -> bool {
        let mut at = lock(&self.recovered);
        if at.is_some_and(|t| t.elapsed() < RECOVER_EVERY) {
            return false;
        }
        *at = Some(Instant::now());
        true
    }

    /// Looks the stream up again on a fresh identity, unless a request
    /// refused at the same time already has: `seen` is the generation the
    /// refused request used.
    fn refresh(&self, server: &Server, seen: u32) -> Result<Upstream> {
        let _one = lock(&self.refreshing);
        {
            let up = lock(&self.upstream);
            if up.generation != seen {
                return Ok(up.clone());
            }
        }
        Ok(self.relook(server, seen)?.0)
    }

    /// The live lookup again, under `refreshing`, and its URLs and identity
    /// swapped in. Returns them, and whether the stream has ended. Never on
    /// a route the user left; one that failed is the answer for `seen`
    /// within `FAILED_FOR`; and it is stopped once the session ends.
    fn relook(&self, server: &Server, seen: u32) -> Result<(Upstream, bool)> {
        let rt = server.runtime();
        if !rt.net.same_route(&self.route) {
            // Looking it up again would send a request over the route they left.
            bail!("The route changed; open the video again");
        }
        if let Some((generation, at, why)) = &*lock(&self.failed)
            && *generation == seen
            && at.elapsed() < FAILED_FOR
        {
            bail!("{why}");
        }
        let abort = || self.is_cancelled();
        let t = Instant::now();
        // A bot check is met on a fresh identity again (see
        // `Server::fresh_on_bot_check`); the URLs are of the one that got through.
        let found = server.fresh_on_bot_check(&rt, &abort, |yt| {
            match yt.live_until(&self.source.watch_url(), self.max_height, &abort)? {
                Opening::Live(s) => Ok((s, yt.net)),
                _ => bail!(NOT_LIVE),
            }
        });
        trace!("relook ms={} ok={}", ms(t), found.is_ok());
        let (s, ident) = match server.guarded(&rt, found) {
            Ok(found) => found,
            Err(e) => {
                if !self.is_cancelled() {
                    *lock(&self.failed) = Some((seen, Instant::now(), format!("{e:#}")));
                }
                return Err(e);
            }
        };
        let fresh = Upstream {
            generation: seen + 1,
            dial: 0,
            video: s.video,
            audio: s.audio,
            headers: s.headers,
            client: ident.relay_client()?,
            ident,
        };
        *lock(&self.upstream) = fresh.clone();
        Ok((fresh, s.ended))
    }
}

/// `X-Head-Seqnum`: the newest segment there is.
fn head_of(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("x-head-seqnum")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// What a segment is served as: WebM video is named `mkv` for FFmpeg, but
/// it is still WebM.
fn content_type(ext: &str) -> &'static str {
    match ext {
        "mkv" => "video/webm",
        "mp4" => "video/mp4",
        _ => "audio/mp4",
    }
}

impl Server {
    /// `GET live/<sid>/video.m3u8` or `audio.m3u8`.
    pub(super) fn live_playlist(&self, out: &mut TcpStream, sid: &str, file: &str) -> Result<()> {
        let kind = if file == "video.m3u8" {
            Kind::Video
        } else {
            Kind::Audio
        };
        match self.live_session(sid) {
            Some(s) => s.serve_playlist(out, kind),
            None => respond(out, 404, "text/plain", b"stream expired"),
        }
    }

    /// `GET live/<sid>/state`.
    pub(super) fn live_state(&self, out: &mut TcpStream, sid: &str) -> Result<()> {
        match self.live_session(sid) {
            Some(s) => s.serve_state(out),
            None => respond(out, 404, "text/plain", b"stream expired"),
        }
    }

    /// `GET live/<sid>/<v|a>/<n>.<ext>`.
    pub(super) fn live_segment(
        self: &Arc<Self>,
        out: &mut TcpStream,
        sid: &str,
        track: &str,
        name: &str,
    ) -> Result<()> {
        match (self.live_session(sid), Kind::named(track)) {
            (Some(s), Some(kind)) => s.serve_segment(self, out, kind, name),
            _ => respond(out, 404, "text/plain", b"stream expired"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: &str = "\
#EXTM3U
#EXT-X-VERSION:7
#EXT-X-TARGETDURATION:5
#EXT-X-PLAYLIST-TYPE:EVENT
#EXT-X-MEDIA-SEQUENCE:10
#EXTINF:5.000,
v/10.mkv
#EXTINF:5.000,
v/11.mkv
#EXTINF:5.000,
v/12.mkv
";

    fn span(ended: bool) -> Span {
        Span {
            oldest: 10,
            head: 12,
            ended,
        }
    }

    #[test]
    fn a_running_playlist_is_an_event_list_of_known_segments() {
        assert_eq!(playlist(&span(false), 5.0, "v/", "mkv"), RUNNING);
    }

    #[test]
    fn an_ended_playlist_ends_with_endlist() {
        assert_eq!(
            playlist(&span(true), 5.0, "v/", "mkv"),
            format!("{RUNNING}#EXT-X-ENDLIST\n")
        );
    }

    #[test]
    fn the_oldest_segment_is_five_days_back_plus_a_margin() {
        assert_eq!(oldest_for(476_821, 5.0), 476_821 - 86_400 + 60);
        // A stream younger than the window lists all of itself.
        assert_eq!(oldest_for(100, 5.0), 0);
        assert_eq!(oldest_for(10, 5.0), 0);
        // And the margin counts from where the window would cut it off.
        assert_eq!(oldest_for(86_400 - 60, 5.0), 0);
        assert_eq!(oldest_for(86_400, 5.0), 60);
    }

    #[test]
    fn a_fragment_list_plays_as_a_finished_video() {
        assert_eq!(
            listed_playlist(&[5.0, 5.0, 2.5], "a/", "m4a"),
            "\
#EXTM3U
#EXT-X-VERSION:7
#EXT-X-TARGETDURATION:5
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:5.000,
a/0.m4a
#EXTINF:5.000,
a/1.m4a
#EXTINF:2.500,
a/2.m4a
#EXT-X-ENDLIST
"
        );
    }

    #[test]
    fn the_target_duration_is_rounded_up() {
        assert!(playlist(&span(false), 5.5, "v/", "mkv").contains("#EXT-X-TARGETDURATION:6\n"));
        assert!(listed_playlist(&[2.0, 6.2], "v/", "mp4").contains("#EXT-X-TARGETDURATION:7\n"));
    }
}

/// A live session served end to end: a `LiveUpstream` for googlevideo, a
/// fake yt-dlp, and the server's own routes.
#[cfg(test)]
mod serving {
    use super::*;
    use crate::app;
    use crate::config::Config;
    use crate::server::testkit::*;
    use crate::server::{MAX_SESSIONS, Session};
    use crate::store::testing::TempDir;
    use serde_json::Value;

    const PLAY: &str = "api/play?v=dQw4w9WgXcQ";

    /// Fields drop in order: the upstream first, which lets go of any
    /// request it holds.
    struct Stream {
        up: LiveUpstream,
        s: Arc<Server>,
        base: String,
        bin: TempDir,
        _d: TempDir,
    }

    fn stream_with(up: LiveUpstream, bin: TempDir, ytdlp: String, cfg: Config) -> Stream {
        let (s, base, d) = server_with(Config { ytdlp, ..cfg });
        Stream {
            up,
            s,
            base,
            bin,
            _d: d,
        }
    }

    /// A stream whose segments `first..=head` exist, growing one every `target_ms`.
    fn stream(target_ms: u64, first: u64, head: u64) -> Stream {
        stream_configured(target_ms, first, head, Config::default())
    }

    fn stream_configured(target_ms: u64, first: u64, head: u64, cfg: Config) -> Stream {
        let up = LiveUpstream::start(target_ms, first, head);
        let bin = TempDir::new();
        let ytdlp = fake_live_ytdlp(&bin.0, &up).display().to_string();
        stream_with(up, bin, ytdlp, cfg)
    }

    impl Stream {
        fn play(&self) -> Value {
            let (status, body) = http(&self.base, "POST", &path(&self.base, PLAY), "");
            assert_eq!(status, 200, "{body}");
            serde_json::from_str(&body).unwrap()
        }

        /// Plays it, and returns the session's id.
        fn open(&self) -> String {
            sid_of(&self.play())
        }

        fn session(&self, sid: &str) -> Arc<LiveSession> {
            self.s.live_session(sid).expect("no such live session")
        }

        fn get(&self, rest: &str) -> Fetched {
            fetch(&self.base, "GET", rest, &[])
        }

        fn segment(&self, sid: &str, track: &str, n: u64) -> Fetched {
            let ext = if track == "v" { "mkv" } else { "m4a" };
            self.get(&format!("live/{sid}/{track}/{n}.{ext}"))
        }

        /// The video playlist's segment numbers, and whether it has ended.
        fn listed(&self, sid: &str) -> (Vec<u64>, bool) {
            let (status, _, body) = self.get(&format!("live/{sid}/video.m3u8"));
            assert_eq!(status, 200);
            entries(&String::from_utf8(body).unwrap())
        }

        /// How many of the upstream's requests were for `target`.
        fn asked(&self, target: &str) -> usize {
            self.up.requests().iter().filter(|r| *r == target).count()
        }

        fn live_lookups(&self) -> u32 {
            std::fs::read_to_string(self.bin.0.join("live-lookups"))
                .map(|s| s.trim().parse().unwrap())
                .unwrap_or(0)
        }
    }

    fn sid_of(v: &Value) -> String {
        let video = v["video"].as_str().expect("no video");
        let rest = video.split("/live/").nth(1).expect("not a live playlist");
        rest.split('/').next().unwrap().to_string()
    }

    /// A playlist's segment numbers, and whether it has ended.
    fn entries(text: &str) -> (Vec<u64>, bool) {
        let numbers = text
            .lines()
            .filter_map(|l| l.strip_prefix("v/"))
            .map(|l| l.strip_suffix(".mkv").unwrap().parse().unwrap())
            .collect();
        (numbers, text.contains("#EXT-X-ENDLIST"))
    }

    /// Segment `n` of a track (`v` or `a`) as the upstream names it, in
    /// its first generation.
    fn sq(track: &str, n: u64) -> String {
        format!("/live/0/{track}&sq={n}")
    }

    #[test]
    fn playing_a_running_stream_answers_two_playlists_and_a_join_point() {
        let st = stream(5_000, 0, 100);
        let v = st.play();
        assert_eq!(v["live"]["state"], "now");
        assert_eq!(v["live"]["segment"], 5.0);
        assert_eq!(v["live"]["started"], 1771008448);
        assert_eq!(v["live"]["watching"], 567);
        assert_eq!(v["live"]["rewind"], true);
        // Three segments back from the end, where mpv starts without waiting.
        assert_eq!(v["start"], -15.0);
        assert!(v.get("waiting").is_none());
        assert_eq!(v["title"], "A live cam");
        let (video, audio) = (v["video"].as_str().unwrap(), v["audio"].as_str().unwrap());
        assert!(video.starts_with(&st.base), "{video}");
        assert!(video.ends_with("/video.m3u8"), "{video}");
        assert!(audio.ends_with("/audio.m3u8"), "{audio}");
        // Both are served, and each lists its own track's segments.
        let sid = sid_of(&v);
        let (status, headers, body) = st.get(rel(&st.base, audio));
        assert_eq!(status, 200);
        assert_eq!(headers["content-type"], "application/vnd.apple.mpegurl");
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("#EXT-X-PLAYLIST-TYPE:EVENT"), "{text}");
        assert!(text.contains("\na/100.m4a\n"), "{text}");
        assert_eq!(st.listed(&sid).0, (0..=100).collect::<Vec<_>>());
    }

    #[test]
    fn the_playlist_lists_only_segments_that_exist_and_grows() {
        let st = stream(300, 0, 10);
        let sid = st.open();
        let (first, ended) = st.listed(&sid);
        assert!(!ended);
        assert_eq!(first[0], 0);
        assert!(*first.last().unwrap() <= st.up.head());
        assert!(first.windows(2).all(|w| w[1] == w[0] + 1), "{first:?}");
        std::thread::sleep(Duration::from_millis(360));
        wait_for("the playlist to grow", Duration::from_secs(5), || {
            let (now, _) = st.listed(&sid);
            // Never a segment that isn't there yet.
            assert!(*now.last().unwrap() <= st.up.head(), "{now:?}");
            now.len() > first.len()
        });
    }

    #[test]
    fn a_segment_is_the_upstreams_bytes_unchanged() {
        let st = stream(60_000, 0, 20);
        let sid = st.open();
        let (status, headers, body) = st.segment(&sid, "v", 5);
        assert_eq!(status, 200);
        assert_eq!(headers["content-type"], "video/webm");
        assert_eq!(headers["content-length"], (64 << 10).to_string());
        assert_eq!(body, live_segment("v", 5));
        let (status, headers, body) = st.segment(&sid, "a", 7);
        assert_eq!(status, 200);
        assert_eq!(headers["content-type"], "audio/mp4");
        assert_eq!(body, live_segment("a", 7));
        // Only by the name the playlist gives it.
        for wrong in ["v/5.webm", "v/5", "a/5.mkv", "v/x.mkv", "x/5.mkv"] {
            assert_eq!(st.get(&format!("live/{sid}/{wrong}")).0, 404, "{wrong}");
        }
        assert_eq!(st.get("live/nope/v/5.mkv").0, 404);
    }

    #[test]
    fn reading_forward_fetches_three_ahead_in_parallel() {
        let st = stream(60_000, 0, 20);
        let sid = st.open();
        assert_eq!(st.segment(&sid, "v", 10).2, live_segment("v", 10));
        wait_for(
            "the next three to be fetched",
            Duration::from_secs(5),
            || (11..=13).all(|n| st.asked(&sq("v", n)) == 1),
        );
        // No more than three, and not the other track's.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(st.asked(&sq("v", 14)), 0);
        assert_eq!(st.asked(&sq("a", 11)), 0);
        // They are what the player gets next, without asking upstream again.
        for n in 11..=13 {
            assert_eq!(st.segment(&sid, "v", n).2, live_segment("v", n));
            assert_eq!(st.asked(&sq("v", n)), 1);
        }
        // Reading on keeps three ahead.
        wait_for("the read-ahead to follow", Duration::from_secs(5), || {
            (14..=16).all(|n| st.asked(&sq("v", n)) == 1)
        });
        // A seek drops it and starts again from there.
        assert_eq!(st.segment(&sid, "v", 2).2, live_segment("v", 2));
        wait_for(
            "the read-ahead after the seek",
            Duration::from_secs(5),
            || (3..=5).all(|n| st.asked(&sq("v", n)) == 1),
        );
        assert_eq!(st.segment(&sid, "v", 3).2, live_segment("v", 3));
        assert_eq!(st.asked(&sq("v", 3)), 1);
    }

    #[test]
    fn a_segment_ahead_of_the_head_waits_for_it() {
        let st = stream(300, 0, 10);
        let sid = st.open();
        let head = *st.listed(&sid).0.last().unwrap();
        // One that comes within two segment lengths is waited for.
        let (status, _, body) = st.segment(&sid, "v", head + 1);
        assert_eq!(status, 200);
        assert_eq!(body, live_segment("v", head + 1));
        // One that doesn't come in that time is not there.
        let asked = Instant::now();
        let (status, _, _) = st.segment(&sid, "v", head + 50);
        assert_eq!(status, 404);
        assert!(
            asked.elapsed() >= Duration::from_millis(550),
            "{:?}",
            asked.elapsed()
        );
        assert!(
            asked.elapsed() < Duration::from_secs(3),
            "{:?}",
            asked.elapsed()
        );
        assert_eq!(st.asked(&sq("v", head + 50)), 0, "never asked upstream");
    }

    #[test]
    fn a_segment_behind_the_window_is_gone_and_the_playlist_moves_on() {
        let st = stream(60_000, 0, 20);
        let sid = st.open();
        assert_eq!(st.listed(&sid).0[0], 0);
        st.up.drop_before(5);
        let (status, _, _) = st.segment(&sid, "v", 2);
        assert_eq!(status, 410);
        let (listed, _) = st.listed(&sid);
        assert_eq!(listed[0], 3, "{listed:?}");
        assert_eq!(*listed.last().unwrap(), 20);
        // One before that is known to be gone, without asking.
        assert_eq!(st.segment(&sid, "a", 1).0, 410);
        assert_eq!(st.asked(&sq("a", 1)), 0);
        // What is still there plays.
        assert_eq!(st.segment(&sid, "v", 6).2, live_segment("v", 6));
    }

    #[test]
    fn a_stream_without_its_old_segments_starts_near_the_head() {
        // Rewinding turned off: only the last few segments are kept.
        let st = stream(60_000, 17, 20);
        let v = st.play();
        assert_eq!(v["live"]["rewind"], false);
        assert_eq!(st.listed(&sid_of(&v)).0, vec![17, 18, 19, 20]);
    }

    #[test]
    fn a_frozen_fragment_is_fetched_again_not_skipped() {
        let st = stream(60_000, 0, 20);
        let sid = st.open();
        st.session(&sid).stall_ms.store(300, Ordering::Relaxed);
        st.up.freeze(10);
        let asked = Instant::now();
        let (status, _, body) = st.segment(&sid, "v", 10);
        assert_eq!(status, 200);
        assert_eq!(body, live_segment("v", 10));
        assert!(asked.elapsed() >= Duration::from_millis(300));
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "{:?}",
            asked.elapsed()
        );
        // The stalled request is still hanging; a second one brought it.
        assert_eq!(st.up.hanging(), 1);
        assert_eq!(st.asked(&sq("v", 10)), 2);
        // And nothing was looked up again for it on Direct: a fresh client
        // keeps the URLs.
        assert_eq!(st.live_lookups(), 1);
    }

    #[test]
    fn a_segment_that_stops_halfway_resumes_where_it_stopped() {
        let st = stream(60_000, 0, 20);
        let sid = st.open();
        st.session(&sid).stall_ms.store(300, Ordering::Relaxed);
        st.up.cut(10, 20_000);
        let asked = Instant::now();
        let (status, headers, body) = st.segment(&sid, "v", 10);
        assert_eq!(status, 200);
        assert_eq!(headers["content-length"], (64 << 10).to_string());
        // Every byte once, in order: none lost or twice at the resume point.
        assert_eq!(body, live_segment("v", 10));
        assert!(asked.elapsed() >= Duration::from_millis(300));
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "{:?}",
            asked.elapsed()
        );
        // The stalled request still hangs; the second asked only for the rest.
        assert_eq!(st.up.hanging(), 1);
        assert_eq!(
            st.up.ranges_of(&sq("v", 10)),
            [None, Some("bytes=20000-".to_string())]
        );
        // On Direct a fresh client keeps the URLs: nothing was looked up.
        assert_eq!(st.live_lookups(), 1);
    }

    #[test]
    fn a_segment_goes_to_the_player_as_it_comes_in() {
        let st = stream(60_000, 0, 20);
        let sid = st.open();
        st.session(&sid).stall_ms.store(1_500, Ordering::Relaxed);
        st.up.cut(10, 20_000);
        let mut c = TcpStream::connect(addr(&st.base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let asked = Instant::now();
        write!(
            c,
            "GET {} HTTP/1.1\r\nHost: x\r\n\r\n",
            path(&st.base, &format!("live/{sid}/v/10.mkv"))
        )
        .unwrap();
        // The head and the bytes before the stall come while it stalls, not
        // after the whole segment is in: the player's clock never runs out.
        let mut raw = Vec::new();
        let body_at = |raw: &[u8]| raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
        while body_at(&raw).is_none_or(|at| raw.len() < at + 20_000) {
            let mut buf = [0u8; 8192];
            let k = c.read(&mut buf).unwrap();
            assert!(k > 0, "the answer ended early");
            raw.extend_from_slice(&buf[..k]);
        }
        let early = asked.elapsed();
        assert!(early < Duration::from_millis(1_200), "{early:?}");
        assert!(raw.starts_with(b"HTTP/1.1 200"));
        c.read_to_end(&mut raw).unwrap();
        assert!(asked.elapsed() >= Duration::from_millis(1_500));
        let at = body_at(&raw).unwrap();
        assert_eq!(raw[at..], live_segment("v", 10));
    }

    #[test]
    fn seeking_again_and_again_leaves_no_more_fetches_running_than_are_held() {
        let st = stream(60_000, 0, 100);
        let sid = st.open();
        // Each segment takes about 6 s to come in.
        st.up.trickle(100);
        for n in [10, 30, 50, 70, 90] {
            // The player asks, and seeks on before the segment is in.
            let mut c = TcpStream::connect(addr(&st.base)).unwrap();
            write!(
                c,
                "GET {} HTTP/1.1\r\nHost: x\r\n\r\n",
                path(&st.base, &format!("live/{sid}/v/{n}.mkv"))
            )
            .unwrap();
            wait_for("its read-ahead to start", Duration::from_secs(5), || {
                (n..=n + 3).all(|m| st.asked(&sq("v", m)) == 1)
            });
            drop(c);
        }
        // Twenty were started; only the last seek's are still wanted: those
        // ahead of it go on, for the player to read on with, and the one it
        // hung up on part way is not fetched for nobody.
        let until = Instant::now() + Duration::from_secs(5);
        while st.up.in_flight() != AHEAD as usize && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        for _ in 0..10 {
            assert_eq!(st.up.in_flight(), AHEAD as usize);
            std::thread::sleep(Duration::from_millis(50));
        }
        // And none that was dropped was asked for again.
        for n in [10, 30, 50, 70, 90] {
            assert_eq!(st.asked(&sq("v", n)), 1, "{n}");
        }
    }

    #[test]
    fn a_stream_that_ends_without_a_recording_gets_endlist_too() {
        let st = stream(200, 0, 10);
        let sid = st.open();
        assert!(!st.listed(&sid).1);
        // The check lookup fails ("recording is not available"): that is
        // how such a stream says it has ended.
        st.up.end_unrecorded();
        let last = st.up.head();
        wait_for("the playlist to end", Duration::from_secs(10), || {
            st.listed(&sid).1
        });
        assert_eq!(st.listed(&sid).0, (0..=last).collect::<Vec<_>>());
        assert!(st.live_lookups() >= 2);
        // The URLs it had still serve what there is.
        assert_eq!(st.segment(&sid, "v", last).2, live_segment("v", last));
    }

    /// What YouTube's answer to an address it is limiting reads like, by
    /// the time yt-dlp has worded it: it starts as a removed video's does.
    const RATE_LIMITED: &str = "ERROR: [youtube] dQw4w9WgXcQ: Video unavailable. This content \
        isn't available, try again later. The current session has been rate-limited by \
        YouTube for up to an hour. It is recommended to use `-t sleep` to add a delay \
        between video requests to avoid exceeding the rate limit.";

    /// A running stream's answer with only an HLS format: nothing to fetch
    /// in fragments, but not over.
    const HLS_ONLY: &str = r#"{"title":"A live cam","live_status":"is_live","is_live":true,
        "formats":[{"format_id":"301","ext":"mp4","protocol":"m3u8_native","vcodec":"avc1.4d401f",
        "acodec":"mp4a.40.2","height":720,"url":"http://127.0.0.1:1/hls.m3u8"}]}"#;

    /// The head stands still until a check lookup has been made and
    /// answered `answer`, and then goes on: the playlist goes on with it,
    /// and never ends.
    fn a_check_that_says_does_not_end_the_stream(answer: &str) {
        let st = stream(200, 0, 10);
        let sid = st.open();
        st.up.lookups_answer(Some(answer));
        st.up.hold_head();
        let lookups = st.live_lookups();
        wait_for("a check lookup", Duration::from_secs(10), || {
            assert!(!st.listed(&sid).1, "it ended");
            st.live_lookups() > lookups
        });
        // Past where it would have ended, with its last poll made.
        for _ in 0..20 {
            assert!(!st.listed(&sid).1, "it ended");
            std::thread::sleep(Duration::from_millis(20));
        }
        let held = st.up.head();
        st.up.release_head();
        wait_for("the playlist to grow again", Duration::from_secs(5), || {
            let (listed, ended) = st.listed(&sid);
            assert!(!ended, "it ended");
            *listed.last().unwrap() > held + 1
        });
        assert_eq!(state(&st, &sid)["state"], "now");
    }

    #[test]
    fn a_rate_limited_end_check_does_not_end_the_stream() {
        a_check_that_says_does_not_end_the_stream(RATE_LIMITED);
    }

    #[test]
    fn an_hls_only_live_answer_does_not_end_the_stream() {
        a_check_that_says_does_not_end_the_stream(HLS_ONLY);
    }

    #[test]
    fn an_ended_answer_with_nothing_to_fetch_ends_the_stream() {
        let st = stream(200, 0, 10);
        let sid = st.open();
        // Over, and offered only as a plain download now.
        st.up.lookups_answer(Some(
            r#"{"title":"A live cam","live_status":"was_live","formats":[{"format_id":"22",
            "ext":"mp4","protocol":"https","vcodec":"avc1","acodec":"mp4a","height":720,
            "url":"http://127.0.0.1:1/v.mp4"}]}"#,
        ));
        st.up.end();
        let last = st.up.head();
        wait_for("the playlist to end", Duration::from_secs(10), || {
            st.listed(&sid).1
        });
        assert_eq!(st.listed(&sid).0, (0..=last).collect::<Vec<_>>());
    }

    #[test]
    fn a_check_that_says_ended_while_the_head_moves_on_does_not_end_it() {
        let st = stream(200, 0, 10);
        let sid = st.open();
        // The lookup says it is over (a stream that kept no recording), but
        // the head goes on as it is asked for one last time.
        st.up.lookups_answer(Some(
            "ERROR: [youtube] x: This live stream recording is not available.",
        ));
        st.up.hold_head();
        let held = st.up.head();
        st.up.release_head_at_the_next_lookup();
        let lookups = st.live_lookups();
        wait_for("a check lookup", Duration::from_secs(10), || {
            assert!(!st.listed(&sid).1, "it ended");
            st.live_lookups() > lookups
        });
        wait_for("the playlist to grow again", Duration::from_secs(5), || {
            let (listed, ended) = st.listed(&sid);
            assert!(!ended, "it ended");
            *listed.last().unwrap() > held + 1
        });
    }

    fn state(st: &Stream, sid: &str) -> Value {
        let (status, headers, body) = st.get(&format!("live/{sid}/state"));
        assert_eq!(status, 200);
        assert_eq!(headers["content-type"], "application/json");
        serde_json::from_slice(&body).unwrap()
    }

    #[test]
    fn the_window_can_ask_whether_the_stream_has_ended() {
        let st = stream(200, 0, 10);
        let sid = st.open();
        let listed = st.listed(&sid).0;
        let now = state(&st, &sid);
        assert_eq!(now["state"], "now");
        assert_eq!(now["oldest"], 0);
        let head = now["head"].as_u64().unwrap();
        assert!(
            head >= *listed.last().unwrap() && head <= st.up.head(),
            "{now}"
        );
        st.up.end();
        // The player goes on reloading the playlist meanwhile.
        wait_for("the state to say ended", Duration::from_secs(10), || {
            st.listed(&sid);
            state(&st, &sid)["state"] == "ended"
        });
        let ended = state(&st, &sid);
        assert_eq!(ended["head"], st.up.head());
        assert_eq!(ended["head"].as_u64(), st.listed(&sid).0.last().copied());
        // A session that is over, or never was, has none.
        assert_eq!(st.get("live/nope/state").0, 404);
        st.session(&sid).cancel();
        assert_eq!(st.get(&format!("live/{sid}/state")).0, 404);
    }

    #[test]
    fn a_403_looks_the_stream_up_again_once() {
        let st = stream(60_000, 0, 20);
        let sid = st.open();
        assert_eq!(runs(&st.bin.0), 2);
        assert_eq!(st.segment(&sid, "v", 5).0, 200);
        wait_for("the read-ahead", Duration::from_secs(5), || {
            st.asked(&sq("v", 8)) == 1
        });
        st.up.forbid_until_relooked();
        // Both tracks are refused together, and share one lookup.
        let fetchers: Vec<_> = ["v", "a"]
            .into_iter()
            .map(|track| {
                let (base, sid) = (st.base.clone(), sid.clone());
                std::thread::spawn(move || {
                    let ext = if track == "v" { "mkv" } else { "m4a" };
                    let got = fetch(&base, "GET", &format!("live/{sid}/{track}/9.{ext}"), &[]);
                    (track, got)
                })
            })
            .collect();
        for f in fetchers {
            let (track, (status, _, body)) = f.join().unwrap();
            assert_eq!(status, 200, "{track}");
            assert_eq!(body, live_segment(track, 9));
        }
        assert_eq!(st.live_lookups(), 2);
        // Only the live lookup: the stream is known to be one.
        assert_eq!(runs(&st.bin.0), 3);
        assert!(st.up.requests().contains(&"/live/1/v&sq=9".to_string()));
        assert_eq!(st.segment(&sid, "v", 15).2, live_segment("v", 15));
        assert_eq!(st.live_lookups(), 2);
    }

    #[test]
    fn an_ended_stream_gets_endlist_and_keeps_every_segment() {
        let st = stream(200, 0, 10);
        let sid = st.open();
        let (before, _) = st.listed(&sid);
        // The user has gone back a little, and plays from there.
        assert_eq!(st.segment(&sid, "v", 2).2, live_segment("v", 2));
        wait_for("the stream to grow", Duration::from_secs(5), || {
            st.listed(&sid).0.len() > before.len() + 2
        });
        st.up.end();
        let last = st.up.head();
        let mut listed = Vec::new();
        wait_for("the playlist to end", Duration::from_secs(10), || {
            let ended;
            (listed, ended) = st.listed(&sid);
            ended
        });
        // Every segment from the first to the real end, no reload needed.
        assert_eq!(listed, (0..=last).collect::<Vec<_>>());
        assert_eq!(st.segment(&sid, "v", 3).2, live_segment("v", 3));
        assert_eq!(st.segment(&sid, "a", last).2, live_segment("a", last));
        // Once it is over, its head is not asked for any more.
        std::thread::sleep(Duration::from_millis(100));
        let heads = |st: &Stream| {
            st.up
                .requests()
                .iter()
                .filter(|r| !r.contains("sq="))
                .count()
        };
        let seen = heads(&st);
        for _ in 0..10 {
            st.listed(&sid);
            std::thread::sleep(Duration::from_millis(60));
        }
        assert_eq!(heads(&st), seen);
    }

    #[test]
    fn nothing_is_fetched_after_the_session_is_cancelled() {
        let st = stream(200, 0, 10);
        let first = st.open();
        let held = st.session(&first);
        assert_eq!(st.segment(&first, "v", 5).0, 200);
        wait_for("the read-ahead", Duration::from_secs(5), || {
            st.asked(&sq("v", 8)) == 1
        });
        // Playing something else ends it.
        let second = st.open();
        assert!(held.cancelled.load(Ordering::Acquire));
        assert_eq!(st.get(&format!("live/{first}/video.m3u8")).0, 404);
        assert_eq!(st.segment(&first, "v", 6).0, 404);
        // And so does a cancel, as eviction or a route change makes it.
        let lookups = st.live_lookups();
        st.session(&second).cancel();
        let seen = st.up.requests().len();
        // Two segment lengths and then some, with the player still asking.
        let until = Instant::now() + Duration::from_millis(600);
        while Instant::now() < until {
            assert_eq!(st.get(&format!("live/{second}/video.m3u8")).0, 404);
            assert_eq!(st.segment(&second, "v", 9).0, 404);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            st.up.requests().len(),
            seen,
            "{:?}",
            &st.up.requests()[seen..]
        );
        assert_eq!(st.live_lookups(), lookups);
    }

    #[test]
    fn a_live_session_is_not_looked_up_again_on_a_route_the_user_left() {
        let st = stream(200, 0, 10);
        let sid = st.open();
        let held = st.session(&sid);
        assert_eq!(runs(&st.bin.0), 2);
        switch_to_tor(&st.base);
        assert!(held.cancelled.load(Ordering::Acquire));
        assert!(st.s.live_session(&sid).is_none());
        let err = held.refresh(&st.s, 0).err().expect("must refuse");
        assert!(err.to_string().contains("route changed"), "{err}");
        let seen = st.up.requests().len();
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(st.up.requests().len(), seen);
        assert_eq!(runs(&st.bin.0), 2, "no lookup went out on the old route");
    }

    #[test]
    fn an_announced_stream_answers_waiting_and_writes_no_history() {
        let up = LiveUpstream::start(60_000, 0, 20);
        let bin = TempDir::new();
        let ytdlp = fake_live_ytdlp_upcoming(&bin.0, &up, 1, 1_800_000_000)
            .display()
            .to_string();
        let cfg = Config {
            history: true,
            ..Config::default()
        };
        let st = stream_with(up, bin, ytdlp, cfg);
        let v = st.play();
        assert_eq!(v["waiting"]["starts"], 1_800_000_000);
        assert_eq!(v["title"], "A live cam");
        assert_eq!(v["channel"], "Cam");
        for key in ["video", "audio", "live", "start"] {
            assert!(v.get(key).is_none(), "{key} in {v}");
        }
        st.s.history_settled();
        assert!(app::load_history(&st.s.paths).unwrap().watched.is_empty());
        assert!(lock(&st.s.sessions).is_empty());
        // Once it runs, it plays, and is in the history like any video.
        let v = st.play();
        assert_eq!(v["live"]["state"], "now");
        st.s.history_settled();
        let watched = app::load_history(&st.s.paths).unwrap().watched;
        assert_eq!(watched.len(), 1);
        assert_eq!(watched[0].title, "A live cam");
    }

    #[test]
    fn preparing_next_skips_live_and_upcoming() {
        let cfg = Config {
            prepare_next: true,
            ..Config::default()
        };
        let st = stream_configured(60_000, 0, 20, cfg);
        let prepare = |query: &str| {
            let (status, body) = http(
                &st.base,
                "POST",
                &path(&st.base, &format!("api/prepare?v=dQw4w9WgXcQ{query}")),
                "",
            );
            assert_eq!(status, 200, "{body}");
            serde_json::from_str::<Value>(&body).unwrap()["lookedUp"].clone()
        };
        // An entry the window knows to be live, or announced, isn't looked up.
        assert_eq!(prepare("&live=now"), false);
        assert_eq!(prepare("&live=upcoming"), false);
        assert_eq!(runs(&st.bin.0), 0);
        // One it didn't know of goes no further than the usual lookup.
        assert_eq!(prepare(""), false);
        assert_eq!(runs(&st.bin.0), 1);
        assert_eq!(st.live_lookups(), 0);
        assert!(st.up.requests().is_empty());
    }

    /// A live stream whose tracks are at `video` and `audio`, numbered.
    fn streams_at(video: &str, audio: &str) -> LiveStreams {
        let track = |base: &str, ext| Track {
            base: base.into(),
            ext,
            format_id: "x".into(),
        };
        LiveStreams {
            title: "T".into(),
            channel: None,
            channel_id: None,
            video: track(video, "mkv"),
            audio: track(audio, "m4a"),
            fragments: Fragments::Numbered,
            target: 5.0,
            started: None,
            watching: None,
            ended: false,
            rewind: true,
            headers: Vec::new(),
        }
    }

    fn session_at(s: &Server, video: &str) -> Arc<LiveSession> {
        let rt = s.runtime();
        Arc::new(
            LiveSession::new(
                test_video(),
                1080,
                rt.net.clone(),
                &streams_at(video, video),
                rt.net.pinned(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn upstream_errors_never_expose_the_signed_url() {
        let (s, base, _d) = server();
        // Nothing listens on port 1, so connecting fails.
        let session = session_at(&s, "http://127.0.0.1:1/videoplayback?sig=SECRETSIG");
        let err = session.begin(&s).unwrap_err();
        assert!(!format!("{err:#}").contains("SECRETSIG"), "{err:#}");
        // And a segment's failure, as the player is told it.
        let sid = s.register(&s.runtime(), Arc::clone(&session)).unwrap();
        let (status, _, body) = fetch(&base, "GET", &format!("live/{sid}/v/0.mkv"), &[]);
        assert_eq!(status, 502);
        let said = String::from_utf8_lossy(&body);
        assert!(!said.is_empty() && !said.contains("SECRETSIG"), "{said}");
    }

    #[test]
    fn a_live_session_counts_against_the_cap_and_is_ended_when_evicted() {
        let st = stream(60_000, 0, 20);
        let sid = st.open();
        let held = st.session(&sid);
        let rt = st.s.runtime();
        let relayed = || {
            let live = crate::server::relay::Live {
                generation: 0,
                dial: 0,
                video: format!("{}/live/0/v&sq=1", st.up.base),
                audio: None,
                headers: Vec::new(),
                client: rt.net.pinned().relay_client().unwrap(),
                served: Arc::default(),
                pool: Arc::default(),
            };
            Session::new(test_video(), "b".into(), rt.net.clone(), live)
        };
        for _ in 0..MAX_SESSIONS - 1 {
            st.s.register(&rt, relayed()).unwrap();
        }
        assert!(st.s.live_session(&sid).is_some());
        assert!(!held.cancelled.load(Ordering::Acquire));
        st.s.register(&rt, relayed()).unwrap();
        assert!(st.s.live_session(&sid).is_none());
        assert!(held.cancelled.load(Ordering::Acquire));
        assert_eq!(lock(&st.s.sessions).len(), MAX_SESSIONS);
    }

    #[test]
    fn a_past_broadcast_plays_as_a_finished_video() {
        let up = LiveUpstream::start(60_000, 0, 20);
        let bin = TempDir::new();
        // A was_live answer whose fragments are listed: three of the
        // upstream's segments, by path.
        let answer = format!(
            r#"{{"title":"Last night","live_status":"was_live","release_timestamp":1771008448,"formats":[
              {{"format_id":"140","ext":"m4a","protocol":"http_dash_segments","vcodec":"none","acodec":"mp4a.40.2",
               "url":"{b}/manifest","fragment_base_url":"{b}/live/0/a&sq=",
               "fragments":[{{"path":"4","duration":5.0}},{{"path":"5","duration":5.0}},{{"path":"6","duration":2.5}}]}},
              {{"format_id":"248","ext":"webm","protocol":"http_dash_segments","vcodec":"vp9","acodec":"none","height":720,
               "url":"{b}/manifest","fragment_base_url":"{b}/live/0/v&sq=",
               "fragments":[{{"path":"4","duration":5.0}},{{"path":"5","duration":5.0}},{{"path":"6","duration":2.5}}]}}]}}"#,
            b = up.base
        );
        std::fs::write(bin.0.join("answer.json"), answer).unwrap();
        let ytdlp = crate::store::testing::script(
            &bin.0.join("yt-dlp"),
            &format!(
                r#"while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && conf="$2"; shift; done
if ! grep -qx -- --live-from-start "$conf"; then
  echo 'ERROR: [youtube] x: This live stream recording is not available.' >&2
  exit 1
fi
cat '{}'"#,
                bin.0.join("answer.json").display()
            ),
        );
        let st = stream_with(up, bin, ytdlp, Config::default());
        let v = st.play();
        assert_eq!(v["live"]["state"], "ended");
        assert!(v.get("start").is_none(), "{v}");
        assert_eq!(v["duration"], 13);
        assert_eq!(v["published"], 1771008448);
        let sid = sid_of(&v);
        let (status, _, body) = st.get(&format!("live/{sid}/video.m3u8"));
        assert_eq!(status, 200);
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("#EXT-X-PLAYLIST-TYPE:VOD"), "{text}");
        assert!(
            text.contains("#EXTINF:2.500,\nv/2.mkv\n#EXT-X-ENDLIST"),
            "{text}"
        );
        // Segment 1 is the second path listed.
        assert_eq!(st.segment(&sid, "v", 1).2, live_segment("v", 5));
        assert_eq!(st.segment(&sid, "a", 2).2, live_segment("a", 6));
        assert_eq!(st.segment(&sid, "v", 3).0, 404);
        // Its head is never polled: it has none.
        assert!(st.up.requests().iter().all(|r| r.contains("sq=")));
        // It is over, as the window learns from its state too.
        assert_eq!(
            state(&st, &sid),
            serde_json::json!({"state":"ended","head":2,"oldest":0})
        );
    }

    /// A stream on an isolating route: through a `Hop`, which keeps the
    /// identity of each connection, with a yt-dlp that meets the bot check
    /// on its first `bot_checks` runs.
    struct OnTor {
        st: Stream,
        hop: Hop,
    }

    fn stream_on_tor(target_ms: u64, first: u64, head: u64, bot_checks: u32) -> OnTor {
        let up = LiveUpstream::start(target_ms, first, head);
        let bin = TempDir::new();
        let inner = fake_live_ytdlp(&bin.0, &up).display().to_string();
        let ytdlp = bot_checked_ytdlp(&bin.0, &inner, bot_checks);
        let hop = Hop::start();
        let cfg = Config {
            proxy: Some(hop.proxy()),
            isolate_circuits: true,
            ..Config::default()
        };
        OnTor {
            st: stream_with(up, bin, ytdlp, cfg),
            hop,
        }
    }

    impl OnTor {
        /// The identities the stream's requests came over.
        fn identities(&self) -> Vec<String> {
            let port = self.st.up.base.rsplit(':').next().unwrap().parse().unwrap();
            self.hop.identities_to(port)
        }

        fn play_live(&self) -> (u16, Value) {
            let target = path(&self.st.base, &format!("{PLAY}&live=now"));
            let (status, body) = http(&self.st.base, "POST", &target, "");
            (status, serde_json::from_str(&body).unwrap())
        }
    }

    #[test]
    fn a_stall_on_tor_is_made_again_on_the_same_identity_first() {
        let t = stream_on_tor(60_000, 0, 20, 0);
        let sid = sid_of(&t.play_live().1);
        // Long enough that a busy machine doesn't stall the request made again.
        t.st.session(&sid).stall_ms.store(1_000, Ordering::Relaxed);
        t.st.up.freeze(10);
        let (status, _, body) = t.st.segment(&sid, "v", 10);
        assert_eq!(status, 200);
        assert_eq!(body, live_segment("v", 10));
        // A fresh connection on the identity the URLs are for: they still
        // work from it, and nothing was looked up again.
        assert_eq!(t.st.asked(&sq("v", 10)), 2);
        assert_eq!(t.st.live_lookups(), 1);
        assert_eq!(t.identities().len(), 1, "{:?}", t.identities());
    }

    #[test]
    fn a_segment_cut_on_tor_resumes_on_the_same_url_and_identity() {
        let t = stream_on_tor(60_000, 0, 20, 0);
        let sid = sid_of(&t.play_live().1);
        // Long enough that a busy machine doesn't stall the request made again.
        t.st.session(&sid).stall_ms.store(1_000, Ordering::Relaxed);
        t.st.up.cut(10, 20_000);
        let (status, _, body) = t.st.segment(&sid, "v", 10);
        assert_eq!(status, 200);
        // Every byte once, in order, of the same cut of the segment.
        assert_eq!(body, live_segment("v", 10));
        assert_eq!(
            t.st.up.ranges_of(&sq("v", 10)),
            [None, Some("bytes=20000-".to_string())]
        );
        assert_eq!(t.st.live_lookups(), 1);
        assert_eq!(t.identities().len(), 1, "{:?}", t.identities());
    }

    #[test]
    fn a_second_stall_in_a_row_on_tor_looks_the_stream_up_again_on_a_fresh_identity() {
        let t = stream_on_tor(60_000, 0, 20, 0);
        let sid = sid_of(&t.play_live().1);
        // Long enough that a busy machine doesn't stall the request made again.
        t.st.session(&sid).stall_ms.store(1_000, Ordering::Relaxed);
        // The circuit is stuck for good: the fresh connection on it stalls too.
        t.st.up.freeze_times(10, 2);
        let asked = Instant::now();
        let (status, _, body) = t.st.segment(&sid, "v", 10);
        assert_eq!(status, 200);
        assert_eq!(body, live_segment("v", 10));
        assert!(asked.elapsed() >= Duration::from_millis(2_000));
        assert_eq!(t.st.asked(&sq("v", 10)), 2);
        assert_eq!(t.st.live_lookups(), 2);
        // The third request was the new lookup's, over its own identity.
        assert!(
            t.st.up.requests().contains(&"/live/1/v&sq=10".to_string()),
            "{:?}",
            t.st.up.requests()
        );
        assert_eq!(t.identities().len(), 2, "{:?}", t.identities());
    }

    #[test]
    fn the_join_segments_come_in_before_the_player_asks() {
        let st = stream(60_000, 0, 20);
        st.s.warming.store(true, Ordering::Relaxed);
        let sid = st.open();
        // The last four of each track, the player's first asks at its join.
        wait_for("the join to come in", Duration::from_secs(5), || {
            (17..=20).all(|n| st.asked(&sq("v", n)) == 1 && st.asked(&sq("a", n)) == 1)
        });
        // FFmpeg reads the first segments when it opens the playlist, before
        // it goes to the join: that doesn't drop them.
        assert_eq!(st.segment(&sid, "v", 0).2, live_segment("v", 0));
        assert_eq!(st.segment(&sid, "a", 0).2, live_segment("a", 0));
        // Then the join is had without asking upstream again, and read on from.
        for n in 18..=20 {
            assert_eq!(st.segment(&sid, "v", n).2, live_segment("v", n));
            assert_eq!(st.segment(&sid, "a", n).2, live_segment("a", n));
            assert_eq!(st.asked(&sq("v", n)), 1, "{n}");
            assert_eq!(st.asked(&sq("a", n)), 1, "{n}");
        }
        // The one before the join, which the player didn't need, was let go.
        assert_eq!(st.asked(&sq("v", 17)), 1);
    }

    #[test]
    fn join_segments_the_head_has_moved_past_are_let_go() {
        let st = stream(300, 0, 20);
        st.s.warming.store(true, Ordering::Relaxed);
        // Each segment takes about 6 s to come in, longer than the head
        // takes to move past them.
        st.up.trickle(100);
        let sid = st.open();
        wait_for("the join to start", Duration::from_secs(5), || {
            st.up.in_flight() == 2 * HELD
        });
        // The player reloads the playlist (and with it the head) while it
        // looks at the first segments.
        wait_for("them to be let go", Duration::from_secs(5), || {
            st.listed(&sid);
            st.up.in_flight() == 0
        });
    }

    #[test]
    fn the_head_is_taken_from_whichever_track_says_it_first() {
        let up = LiveUpstream::start(60_000, 0, 20);
        // The video's host doesn't answer; the audio's does.
        up.hold_base("v");
        let bin = TempDir::new();
        let ytdlp = fake_live_ytdlp(&bin.0, &up).display().to_string();
        let st = stream_with(up, bin, ytdlp, Config::default());
        let asked = Instant::now();
        let v = st.play();
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "{:?}",
            asked.elapsed()
        );
        assert_eq!(v["live"]["rewind"], true);
        assert_eq!(*st.listed(&sid_of(&v)).0.last().unwrap(), 20);
    }

    #[test]
    fn a_player_that_hangs_up_part_way_stops_that_fetch_at_once() {
        let st = stream(60_000, 0, 100);
        let sid = st.open();
        // Each segment takes about 6 s to come in.
        st.up.trickle(100);
        let mut c = TcpStream::connect(addr(&st.base)).unwrap();
        write!(
            c,
            "GET {} HTTP/1.1\r\nHost: x\r\n\r\n",
            path(&st.base, &format!("live/{sid}/v/5.mkv"))
        )
        .unwrap();
        let mut b = [0u8; 4096];
        c.read_exact(&mut b).unwrap();
        wait_for("the read-ahead", Duration::from_secs(5), || {
            st.up.in_flight() == HELD
        });
        drop(c);
        // The one it hung up on stops within a poll or two; those ahead go on.
        let gone = Instant::now();
        wait_for("its fetch to stop", Duration::from_secs(3), || {
            st.up.in_flight() == AHEAD as usize
        });
        assert!(
            gone.elapsed() < Duration::from_secs(2),
            "{:?}",
            gone.elapsed()
        );
        // Asked for again, it is fetched anew, from the start.
        st.up.trickle(0);
        assert_eq!(st.segment(&sid, "v", 5).2, live_segment("v", 5));
        assert_eq!(st.asked(&sq("v", 5)), 2);
    }

    #[test]
    fn a_stream_the_card_says_is_live_skips_the_usual_lookup() {
        let st = stream(60_000, 0, 20);
        let target = path(&st.base, &format!("{PLAY}&live=now"));
        let (status, body) = http(&st.base, "POST", &target, "");
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["live"]["state"], "now");
        // One run, and it was the live lookup.
        assert_eq!(runs(&st.bin.0), 1);
        assert_eq!(st.live_lookups(), 1);
    }

    #[test]
    fn a_stream_a_list_said_was_live_skips_the_usual_lookup() {
        let st = stream(60_000, 0, 20);
        st.s.marks
            .found("dQw4w9WgXcQ", Some(crate::video::LiveState::Now));
        assert_eq!(st.play()["live"]["state"], "now");
        assert_eq!(runs(&st.bin.0), 1);
        // And one it said nothing of is looked up the usual way first.
        let other = stream(60_000, 0, 20);
        assert_eq!(other.play()["live"]["state"], "now");
        assert_eq!(runs(&other.bin.0), 2);
        // Once a play has found it live, playing it again skips it too.
        assert_eq!(other.play()["live"]["state"], "now");
        assert_eq!(runs(&other.bin.0), 3);
    }

    #[test]
    fn a_stream_the_card_says_is_coming_is_asked_once() {
        let up = LiveUpstream::start(60_000, 0, 20);
        let bin = TempDir::new();
        let ytdlp = fake_live_ytdlp_upcoming(&bin.0, &up, 1, 1_800_000_000)
            .display()
            .to_string();
        let st = stream_with(up, bin, ytdlp, Config::default());
        let target = path(&st.base, &format!("{PLAY}&live=upcoming"));
        let (status, body) = http(&st.base, "POST", &target, "");
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["waiting"]["starts"], 1_800_000_000);
        assert_eq!(runs(&st.bin.0), 1);
    }

    /// A yt-dlp whose live lookup finds an ordinary video, which the usual
    /// lookup plays from `url`, and which counts its runs.
    fn ordinary_ytdlp(dir: &std::path::Path, url: &str) -> String {
        let script = r#"n=$(cat '@DIR@/runs' 2>/dev/null || echo 0)
echo $((n + 1)) > '@DIR@/runs.new' && mv '@DIR@/runs.new' '@DIR@/runs'
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && conf="$2"; shift; done
if grep -qx -- --live-from-start "$conf"; then
  echo '{"title":"T","live_status":"not_live","formats":[{"format_id":"18","ext":"mp4","protocol":"https","vcodec":"avc1","acodec":"mp4a","height":360,"url":"@URL@"}]}'
else
  echo '{"title":"T","requested_formats":[{"url":"@URL@","protocol":"https","vcodec":"avc1"},{"url":"@URL@","protocol":"https","vcodec":"none"}]}'
fi
"#
        .replace("@DIR@", &dir.display().to_string())
        .replace("@URL@", url);
        crate::store::testing::script(&dir.join("yt-dlp"), &script)
    }

    #[test]
    fn a_card_that_wrongly_says_live_still_plays_the_video() {
        let bin = TempDir::new();
        let ytdlp = ordinary_ytdlp(&bin.0, "http://127.0.0.1:1/v");
        let (_s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let target = path(&base, &format!("{PLAY}&live=now"));
        let (status, body) = http(&base, "POST", &target, "");
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        // Played as the video it is, from the usual lookup after the live one.
        assert!(v.get("live").is_none(), "{v}");
        assert!(v["video"].as_str().unwrap().contains("/s/"), "{v}");
        assert_eq!(runs(&bin.0), 2);
    }

    #[test]
    fn a_bot_check_on_tor_is_asked_again_on_fresh_circuits() {
        let t = stream_on_tor(60_000, 0, 20, 2);
        let (status, v) = t.play_live();
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["live"]["state"], "now");
        // Three runs, each on an identity of its own.
        let ran = identities_of(&t.st.bin.0);
        assert_eq!(ran.len(), 3, "{ran:?}");
        assert!(ran.iter().all(|w| !w.is_empty()), "{ran:?}");
        assert!(
            ran[0] != ran[1] && ran[1] != ran[2] && ran[0] != ran[2],
            "{ran:?}"
        );
        // The stream is fetched on the one that got through, and only on it.
        let sid = sid_of(&v);
        assert_eq!(t.st.segment(&sid, "v", 10).0, 200);
        assert_eq!(t.identities(), [ran[2].clone()]);
    }

    #[test]
    fn three_bot_checks_in_a_row_are_the_answer() {
        let t = stream_on_tor(60_000, 0, 20, 3);
        let (status, v) = t.play_live();
        assert_eq!(status, 500, "{v}");
        assert_eq!(v["code"], "bot_check", "{v}");
        assert_eq!(identities_of(&t.st.bin.0).len(), 3);
        assert!(t.identities().is_empty());
    }

    #[test]
    fn a_bot_check_on_a_route_that_does_not_isolate_is_the_answer_at_once() {
        let up = LiveUpstream::start(60_000, 0, 20);
        let bin = TempDir::new();
        let inner = fake_live_ytdlp(&bin.0, &up).display().to_string();
        let ytdlp = bot_checked_ytdlp(&bin.0, &inner, 1);
        let st = stream_with(up, bin, ytdlp, Config::default());
        let target = path(&st.base, &format!("{PLAY}&live=now"));
        let (status, body) = http(&st.base, "POST", &target, "");
        assert_eq!(status, 500, "{body}");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["code"],
            "bot_check"
        );
        assert_eq!(identities_of(&st.bin.0).len(), 1);
    }

    #[test]
    fn a_bot_check_on_tor_is_asked_again_for_an_ordinary_video_too() {
        let bin = TempDir::new();
        let inner = fake_ytdlp(&bin.0, "http://127.0.0.1:1/v");
        let ytdlp = bot_checked_ytdlp(&bin.0, &inner, 1);
        let (_s, base, _d) = server_with(Config {
            ytdlp,
            proxy: Some("socks5h://127.0.0.1:9".into()),
            isolate_circuits: true,
            ..Config::default()
        });
        let (status, body) = http(&base, "POST", &path(&base, PLAY), "");
        assert_eq!(status, 200, "{body}");
        let ran = identities_of(&bin.0);
        assert_eq!(ran.len(), 2, "{ran:?}");
        assert_ne!(ran[0], ran[1]);
        assert_eq!(runs(&bin.0), 1);
    }
}
