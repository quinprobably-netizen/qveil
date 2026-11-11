//! The start and the end of a session's streams, fetched as soon as it is
//! opened.
//!
//! mpv opens a video with a handful of Range requests one after the other:
//! the start of the video stream, its end (where an MP4 may keep its index),
//! the start again, and the start of the audio. Each is a round trip through
//! the route, which over Tor is a good part of a second. Those are the same
//! bytes every time, so the relay asks for them itself, in parallel, while
//! the window is still getting the answer to `play` and starting mpv, and
//! keeps them here for the player's requests to be answered from.
//!
//! What is kept is bounded (`HEAD` and `TAIL` per stream, a few MiB a
//! session, and there are at most `MAX_SESSIONS`), belongs to one resolve of
//! the stream (a refresh makes it stale), and is dropped when the session is
//! cancelled. A failure here costs nothing: the player's request then goes
//! upstream as it would have anyway.

use super::Server;
use super::relay::{Live, Session, content_range, fetch_upstream, is_playlist_type};
use crate::util::lock;
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use std::io::{self, Read};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// How much of the start of each stream is fetched.
pub(super) const HEAD: u64 = 1 << 20;
/// How much of the end.
pub(super) const TAIL: u64 = 64 << 10;
/// How often a request waiting for a piece that is on its way looks at the
/// cancel flag.
const POLL: Duration = Duration::from_millis(250);

/// Bytes `start..start + bytes.len()` of a stream `total` bytes long.
#[derive(Clone, Debug)]
pub(super) struct Piece {
    pub(super) start: u64,
    pub(super) bytes: Arc<[u8]>,
    pub(super) total: u64,
    pub(super) content_type: Option<String>,
    /// The resolve it was fetched from (`Live::generation`).
    generation: u32,
}

impl Piece {
    fn holds(&self, pos: u64) -> bool {
        pos >= self.start && pos < self.start + self.bytes.len() as u64
    }

    /// The last byte it has.
    pub(super) fn last(&self) -> u64 {
        self.start + self.bytes.len() as u64 - 1
    }
}

#[derive(Clone, Debug, Default)]
enum Slot {
    /// Not asked for, or it failed.
    #[default]
    Empty,
    Coming,
    Here(Piece),
}

#[derive(Default)]
struct Stream {
    head: Slot,
    tail: Slot,
    /// Where the tail starts, once that is known: from the length yt-dlp
    /// reported, or else from the head's response.
    tail_from: Option<u64>,
}

#[derive(Default)]
struct State {
    video: Stream,
    audio: Stream,
    /// Set when the session is cancelled: nothing more is fetched or kept.
    stopped: bool,
}

impl State {
    fn stream(&mut self, kind: &str) -> &mut Stream {
        if kind == "v" {
            &mut self.video
        } else {
            &mut self.audio
        }
    }
}

#[derive(Default)]
pub(super) struct Warm {
    state: Mutex<State>,
    changed: Condvar,
}

impl Warm {
    fn wait<'a>(&self, st: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        match self.changed.wait_timeout(st, POLL) {
            Ok((st, _)) => st,
            Err(e) => e.into_inner().0,
        }
    }

    /// Drops what is kept, and stops what is being fetched.
    pub(super) fn stop(&self) {
        let mut st = lock(&self.state);
        *st = State {
            stopped: true,
            ..State::default()
        };
        self.changed.notify_all();
    }

    /// The piece of stream `kind` that holds byte `pos`, from the resolve
    /// `generation`. A piece that is on its way and may hold it is waited
    /// for, rather than asked for a second time.
    pub(super) fn find(&self, s: &Session, kind: &str, pos: u64, generation: u32) -> Option<Piece> {
        let mut st = lock(&self.state);
        loop {
            if st.stopped || s.cancelled.load(Ordering::Acquire) {
                return None;
            }
            let (head, tail, tail_from) = {
                let stream = st.stream(kind);
                (stream.head.clone(), stream.tail.clone(), stream.tail_from)
            };
            let fits = |p: &Piece| p.holds(pos) && p.generation == generation;
            let in_tail = tail_from.is_some_and(|from| pos >= from);
            match head {
                Slot::Here(p) if fits(&p) => return Some(p),
                // It may hold `pos`, or say where the tail starts.
                Slot::Coming if !in_tail => {
                    st = self.wait(st);
                    continue;
                }
                _ => {}
            }
            match tail {
                Slot::Coming if in_tail => st = self.wait(st),
                Slot::Here(p) if fits(&p) => return Some(p),
                _ => return None,
            }
        }
    }

    /// Puts `piece` in its place, unless the session was stopped meanwhile.
    /// With `tail_from`, the tail is marked as on its way in the same step,
    /// so that nobody looks in between and decides not to wait for it.
    fn put(&self, kind: &str, head: bool, piece: Option<Piece>, tail_from: Option<u64>) -> bool {
        let mut st = lock(&self.state);
        if st.stopped {
            return false;
        }
        let slot = match piece {
            Some(p) => Slot::Here(p),
            None => Slot::Empty,
        };
        let stream = st.stream(kind);
        if head {
            stream.head = slot;
        } else {
            stream.tail = slot;
        }
        if let Some(from) = tail_from {
            stream.tail = Slot::Coming;
            stream.tail_from = Some(from);
        }
        self.changed.notify_all();
        true
    }

    /// Marks the heads of `kinds` as on their way, and the tails of those
    /// whose start is known. False once stopped.
    fn expect(&self, kinds: &[(&str, Option<u64>)]) -> bool {
        let mut st = lock(&self.state);
        if st.stopped {
            return false;
        }
        for (kind, tail_from) in kinds {
            let stream = st.stream(kind);
            stream.head = Slot::Coming;
            if let Some(from) = tail_from {
                stream.tail = Slot::Coming;
                stream.tail_from = Some(*from);
            }
        }
        true
    }
}

/// Where the tail of a stream `total` bytes long starts, if it has one that
/// the head doesn't cover.
fn tail_of(total: u64) -> Option<u64> {
    Some(total.saturating_sub(TAIL)).filter(|&from| from > HEAD)
}

/// Starts fetching the head and the tail of each stream of `s`. `sizes` are
/// the lengths of the video and the audio stream, where yt-dlp knew them:
/// then the tail is fetched alongside the head, and otherwise once the
/// head's response has said how long the stream is.
pub(super) fn start(server: &Arc<Server>, s: &Arc<Session>, sizes: [Option<u64>; 2]) {
    let live = lock(&s.live).clone();
    let mut kinds = vec![("v", sizes[0].and_then(tail_of))];
    if live.audio.is_some() {
        kinds.push(("a", sizes[1].and_then(tail_of)));
    }
    if !s.warm.expect(&kinds) {
        return;
    }
    let spawn = |kind: &'static str, part: Part| {
        let (server, session, live) = (Arc::clone(server), Arc::clone(s), live.clone());
        let spawned = std::thread::Builder::new()
            .spawn(move || fetch_part(&server, &session, &live, kind, part));
        if spawned.is_err() {
            // Nobody would ever fill it, and a request would wait for it.
            s.warm
                .put(kind, matches!(part, Part::Head { .. }), None, None);
        }
    };
    for (kind, tail_from) in kinds {
        spawn(
            kind,
            Part::Head {
                then_tail: tail_from.is_none(),
            },
        );
        if let Some(from) = tail_from {
            spawn(kind, Part::Tail(from));
        }
    }
}

#[derive(Clone, Copy)]
enum Part {
    /// The head, and then the tail if `then_tail` (its start wasn't known).
    Head { then_tail: bool },
    /// The tail, from this byte.
    Tail(u64),
}

fn fetch_part(server: &Server, s: &Session, live: &Live, kind: &str, part: Part) {
    let piece = |start, last| match piece(s, live, kind, start, last) {
        Ok(found) => found,
        Err(Stalled) => {
            // The player's own request then goes upstream, on a new client.
            server.redial(s, live);
            None
        }
    };
    match part {
        Part::Head { then_tail } => {
            let head = piece(0, HEAD - 1);
            let tail_from = head
                .as_ref()
                .filter(|_| then_tail)
                .and_then(|h| tail_of(h.total));
            if s.warm.put(kind, true, head, tail_from)
                && let Some(from) = tail_from
            {
                let tail = piece(from, from + TAIL - 1);
                s.warm.put(kind, false, tail, None);
            }
        }
        Part::Tail(from) => {
            let tail = piece(from, from + TAIL - 1);
            s.warm.put(kind, false, tail, None);
        }
    }
}

/// A warm-up request timed out, or its connection failed.
struct Stalled;

/// Bytes `start..=last` of stream `kind`, or `None` if they can't be had
/// as they are (a 403 is left to the player's own request, which resolves
/// the stream again).
fn piece(
    s: &Session,
    live: &Live,
    kind: &str,
    start: u64,
    last: u64,
) -> Result<Option<Piece>, Stalled> {
    let range = format!("bytes={start}-{last}");
    let Some(mut up) =
        fetch_upstream(live, &s.route, "GET", kind, Some(&range)).map_err(|_| Stalled)?
    else {
        return Ok(None);
    };
    if up.status() != StatusCode::PARTIAL_CONTENT {
        return Ok(None);
    }
    let content_type = up
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    if content_type.as_deref().is_some_and(is_playlist_type) {
        return Ok(None);
    }
    let Some((a, b, total)) = content_range(&up) else {
        return Ok(None);
    };
    if a != start || b > last {
        return Ok(None);
    }
    let want = (b + 1 - a) as usize;
    let mut bytes = Vec::with_capacity(want);
    let mut buf = vec![0u8; 64 << 10];
    while bytes.len() < want {
        if s.cancelled.load(Ordering::Acquire) {
            return Ok(None);
        }
        match up.read(&mut buf) {
            Ok(0) => return Ok(None),
            Ok(n) => bytes.extend_from_slice(&buf[..n.min(want - bytes.len())]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Stalled),
        }
    }
    Ok(Some(Piece {
        start,
        bytes: bytes.into(),
        total,
        content_type,
        generation: live.generation,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::server::testkit::*;
    use crate::store::testing::TempDir;
    use std::sync::Arc;
    use std::time::Duration;

    const SIZE: u64 = 3_000_000;

    /// A server with warming on, whose yt-dlp resolves both streams of a
    /// video to `path` on `up`, with the number of the resolve appended.
    fn warm_server(
        up: &Upstream,
        path: &str,
    ) -> (Arc<crate::server::Server>, String, TempDir, TempDir) {
        let bin = TempDir::new();
        let ytdlp = fake_ytdlp(&bin.0, &up.url(path));
        let (s, base, d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        s.warming.store(true, Ordering::Relaxed);
        (s, base, d, bin)
    }

    #[test]
    fn mpvs_first_requests_are_answered_from_what_was_fetched_at_open() {
        let up = Upstream::start();
        // Slow enough that the player's first request arrives while the
        // head is still on its way.
        let (s, base, _d, _bin) = warm_server(&up, &format!("/big?size={SIZE}&pace=2&run="));
        let info = s.open(&test_video()).unwrap();
        let (video, audio) = (
            rel(&base, &info.video),
            rel(&base, info.audio.as_deref().unwrap()),
        );
        let get = |stream: &str, range: &str| fetch(&base, "GET", stream, &[("Range", range)]);

        // The start, the end, the start again and the audio's start, as mpv
        // asks for them when it opens an MP4.
        let (status, headers, body) = get(video, "bytes=0-99");
        assert_eq!(
            (status, headers["content-range"].as_str()),
            (206, &*format!("bytes 0-99/{SIZE}"))
        );
        assert_eq!(headers["content-type"], "video/mp4");
        assert_eq!(body, big_bytes(0, 100));
        let tail = SIZE - 1000;
        let (status, headers, body) = get(video, &format!("bytes={tail}-"));
        assert_eq!(status, 206);
        assert_eq!(
            headers["content-range"],
            format!("bytes {tail}-{}/{SIZE}", SIZE - 1)
        );
        assert_eq!(body, big_bytes(tail, SIZE));
        assert_eq!(get(video, "bytes=500-999").2, big_bytes(500, 1000));
        assert_eq!(get(audio, "bytes=0-4095").2, big_bytes(0, 4096));
        settled(&up);
        let head = format!("bytes=0-{}", HEAD - 1);
        let tail = format!("bytes={}-{}", SIZE - TAIL, SIZE - 1);
        let mut asked = ranges(&up);
        asked.sort();
        assert_eq!(
            asked,
            [head.clone(), head, tail.clone(), tail],
            "the player's requests went upstream"
        );

        // Playing on from the start takes the head from memory and fetches
        // what follows it straight away.
        let (status, _, body) = get(video, "bytes=0-");
        assert_eq!(status, 206);
        assert!(body == big_bytes(0, SIZE), "the bytes differ");
        assert_eq!(ranges(&up)[4..], [format!("bytes={HEAD}-{}", SIZE - 1)]);
    }

    #[test]
    fn with_the_length_known_the_tail_does_not_wait_for_the_head() {
        let up = Upstream::start();
        let bin = TempDir::new();
        // At this pace the head takes over a second, and the tail a tenth of that.
        let url = up.url(&format!("/big?size={SIZE}&pace=20"));
        let ytdlp = crate::store::testing::script(
            &bin.0.join("yt-dlp"),
            &format!(
                r#"echo '{{"title":"T","url":"{url}","protocol":"https","vcodec":"avc1","filesize":{SIZE}}}'"#
            ),
        );
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        s.warming.store(true, Ordering::Relaxed);
        let info = s.open(&test_video()).unwrap();
        let started = std::time::Instant::now();
        let tail = SIZE - 100;
        let (status, _, body) = fetch(
            &base,
            "GET",
            rel(&base, &info.video),
            &[("Range", &format!("bytes={tail}-"))],
        );
        assert_eq!((status, body), (206, big_bytes(tail, SIZE)));
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "{:?}",
            started.elapsed()
        );
        settled(&up);
        let mut asked = ranges(&up);
        asked.sort();
        assert_eq!(
            asked,
            [
                format!("bytes=0-{}", HEAD - 1),
                format!("bytes={}-{}", SIZE - TAIL, SIZE - 1)
            ]
        );
    }

    #[test]
    fn what_was_fetched_is_not_used_after_the_stream_is_resolved_again() {
        let up = Upstream::start();
        // `gen=1` answers the head and then goes stale, so the tail and
        // anything after the head get a 403.
        let (s, base, _d, bin) = warm_server(&up, &format!("/big?size={SIZE}&gen="));
        let info = s.open(&test_video()).unwrap();
        let video = rel(&base, &info.video);
        let get = |range: &str| fetch(&base, "GET", video, &[("Range", range)]);
        assert_eq!(get("bytes=0-99").2, big_bytes(0, 100));
        settled(&up);
        let before = up.requests().len();
        // Past the head: a 403, a fresh resolve, and the bytes.
        assert_eq!(
            get("bytes=2000000-2000099").2,
            big_bytes(2_000_000, 2_000_100)
        );
        assert_eq!(runs(&bin.0), 2);
        // The head now comes from the new resolve, not from memory.
        assert_eq!(get("bytes=0-99").2, big_bytes(0, 100));
        let after: Vec<_> = up.requests()[before..]
            .iter()
            .map(|(t, r)| format!("{t} {}", r.clone().unwrap_or_default()))
            .collect();
        let url = |n: u32| format!("/big?size={SIZE}&gen={n}");
        assert_eq!(
            after,
            [
                format!("{} bytes=2000000-2000099", url(1)),
                format!("{} bytes=2000000-2000099", url(2)),
                format!("{} bytes=0-99", url(2)),
            ]
        );
    }

    #[test]
    fn a_route_switch_stops_the_fetching_and_drops_what_was_fetched() {
        let up = Upstream::start();
        // At this pace the head takes about two seconds.
        let (s, base, _d, _bin) = warm_server(&up, &format!("/big?size={SIZE}&pace=30&run="));
        let info = s.open(&test_video()).unwrap();
        let sid = rel(&base, &info.video)
            .split('/')
            .nth(1)
            .unwrap()
            .to_string();
        let held = s.session(&sid).unwrap();
        wait_for("the head to be on its way", Duration::from_secs(5), || {
            up.gauge.live.load(Ordering::SeqCst) == 2
        });
        switch_to_tor(&base);
        wait_for("the upstream to be let go", Duration::from_secs(5), || {
            up.gauge.live.load(Ordering::SeqCst) == 0
        });
        assert_eq!(settled(&up), 2, "the tails were asked for after the switch");
        assert!(held.warm.find(&held, "v", 0, 0).is_none());
        assert!(lock(&held.warm.state).stopped);
    }
}
