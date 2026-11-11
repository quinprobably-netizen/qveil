//! The start of a session's streams, fetched as soon as it is opened.
//!
//! mpv opens a video with a handful of Range requests one after the other,
//! and the first of each stream is for its start. Each is a round trip
//! through the route, which over Tor is a good part of a second. So the
//! relay asks for the start of each stream itself, while the window is
//! still getting the answer to `play` and starting mpv, and a player's
//! request that starts there is answered from that request: its bytes are
//! passed on as they arrive, so the player's first byte comes one round trip
//! after the open, not once the whole head is in.
//!
//! What is kept is bounded (`HEAD` of the video, `AUDIO_HEAD` of the audio,
//! and there are at most `MAX_SESSIONS`), belongs to one resolve of the
//! stream (a refresh makes it stale), and is dropped when the session is
//! cancelled. A failure here costs nothing: the player's request then goes
//! upstream as it would have anyway, and so does one that finds the head
//! stalled (no response within `HEAD_WAIT`).
//!
//! The end of a stream (where an MP4 may keep its index) is not fetched:
//! YouTube's streams keep their index at the start, and mpv never asked for
//! the end of one.

use super::Server;
use super::relay::{Live, Session, content_range, fetch_upstream, is_playlist_type};
use crate::util::lock;
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use std::io::{self, Read};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How much of the start of the video is fetched: enough for the player to
/// start, and for what follows to be on its way before it runs out.
pub(super) const HEAD: u64 = 1 << 20;
/// How much of the start of the audio. Audio is a tenth of the video's
/// bitrate, and on one circuit every byte of it is a byte the video's first
/// frame waits for.
pub(super) const AUDIO_HEAD: u64 = 128 << 10;
/// How long a player's request waits for the head's response to start
/// before it goes upstream itself, and how long it waits for the next bytes
/// of the head before it fetches the rest itself.
pub(super) const HEAD_WAIT: Duration = Duration::from_secs(3);
/// How often a request waiting for the head looks at the cancel flag.
const POLL: Duration = Duration::from_millis(250);

/// The start of one stream, as it arrives: bytes `0..bytes.len()` of a
/// stream `total` bytes long, of which `0..=last` are on their way.
pub(super) struct Head {
    /// The resolve it is fetched from (`Live::generation`).
    generation: u32,
    state: Mutex<HeadState>,
    changed: Condvar,
}

#[derive(Default)]
struct HeadState {
    /// What the response said, once it has started.
    meta: Option<Meta>,
    bytes: Vec<u8>,
    /// Set once nothing more will arrive: all of it did, or the fetch
    /// failed, was stopped or was given up on.
    ended: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Meta {
    /// The last byte it will have.
    pub(super) last: u64,
    pub(super) total: u64,
    pub(super) content_type: Option<String>,
}

impl Head {
    fn new(generation: u32) -> Arc<Self> {
        Arc::new(Head {
            generation,
            state: Mutex::default(),
            changed: Condvar::new(),
        })
    }

    fn wait<'a>(&self, st: MutexGuard<'a, HeadState>) -> MutexGuard<'a, HeadState> {
        match self.changed.wait_timeout(st, POLL) {
            Ok((st, _)) => st,
            Err(e) => e.into_inner().0,
        }
    }

    /// Nothing more arrives: whoever waits for it stops waiting.
    fn end(&self) {
        lock(&self.state).ended = true;
        self.changed.notify_all();
    }

    fn ended(&self) -> bool {
        lock(&self.state).ended
    }

    /// What the response said, once it has started; `None` if it failed, or
    /// didn't start within `HEAD_WAIT`, or `stop` said so first.
    fn meta(&self, stop: &dyn Fn() -> bool) -> Option<Meta> {
        let deadline = Instant::now() + HEAD_WAIT;
        let mut st = lock(&self.state);
        loop {
            if let Some(m) = &st.meta {
                return Some(m.clone());
            }
            if st.ended || stop() || Instant::now() >= deadline {
                return None;
            }
            st = self.wait(st);
        }
    }

    /// Appends the bytes from `pos` up to `upto` (inclusive) to `out` as
    /// they arrive, at most `max` at a time, and returns how many; 0 once
    /// there are no more to be had: it ended short of `pos`, `stop` said
    /// so, or nothing came for `HEAD_WAIT`.
    pub(super) fn read_at(
        &self,
        pos: u64,
        upto: u64,
        max: usize,
        out: &mut Vec<u8>,
        stop: &dyn Fn() -> bool,
    ) -> usize {
        let mut deadline = Instant::now() + HEAD_WAIT;
        let mut st = lock(&self.state);
        loop {
            let have = st.bytes.len() as u64;
            if pos > upto {
                return 0;
            }
            if pos < have {
                let to = have.min(upto + 1).min(pos + max as u64);
                out.extend_from_slice(&st.bytes[pos as usize..to as usize]);
                return (to - pos) as usize;
            }
            if st.ended || stop() {
                return 0;
            }
            let before = st.bytes.len();
            st = self.wait(st);
            if st.bytes.len() != before {
                deadline = Instant::now() + HEAD_WAIT;
            } else if Instant::now() >= deadline {
                return 0;
            }
        }
    }
}

#[derive(Default)]
struct State {
    video: Option<Arc<Head>>,
    audio: Option<Arc<Head>>,
    /// Set when the session is cancelled: nothing more is fetched or kept.
    stopped: bool,
}

impl State {
    fn stream(&mut self, kind: &str) -> &mut Option<Arc<Head>> {
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
}

impl Warm {
    /// Drops what is kept, and stops what is being fetched.
    pub(super) fn stop(&self) {
        let gone = {
            let mut st = lock(&self.state);
            st.stopped = true;
            [st.video.take(), st.audio.take()]
        };
        for head in gone.into_iter().flatten() {
            head.end();
        }
    }

    fn stopped(&self) -> bool {
        lock(&self.state).stopped
    }

    /// The head of stream `kind`, from the resolve `generation`, if it holds
    /// or will hold byte `pos`: once its response has started, which is
    /// waited for (up to `HEAD_WAIT`) rather than asked for a second time.
    pub(super) fn find(
        &self,
        s: &Session,
        kind: &str,
        pos: u64,
        generation: u32,
    ) -> Option<(Arc<Head>, Meta)> {
        let head = lock(&self.state).stream(kind).clone()?;
        if head.generation != generation {
            return None;
        }
        let stop = || self.stopped() || s.cancelled.load(Ordering::Acquire);
        let meta = head.meta(&stop)?;
        (pos <= meta.last).then_some((head, meta))
    }

    /// Puts a head on its way for each of `kinds`, unless stopped.
    fn expect(&self, kinds: &[&str], generation: u32) -> Option<Vec<Arc<Head>>> {
        let mut st = lock(&self.state);
        if st.stopped {
            return None;
        }
        Some(
            kinds
                .iter()
                .map(|kind| {
                    let head = Head::new(generation);
                    *st.stream(kind) = Some(Arc::clone(&head));
                    head
                })
                .collect(),
        )
    }
}

/// Starts fetching the head of each stream of `s`.
pub(super) fn start(server: &Arc<Server>, s: &Arc<Session>) {
    let live = lock(&s.live).clone();
    let mut kinds = vec!["v"];
    if live.audio.is_some() {
        kinds.push("a");
    }
    let Some(heads) = s.warm.expect(&kinds, live.generation) else {
        return;
    };
    for (kind, head) in kinds.into_iter().zip(heads) {
        let (server, session, live, fill) = (
            Arc::clone(server),
            Arc::clone(s),
            live.clone(),
            Arc::clone(&head),
        );
        let spawned = std::thread::Builder::new().spawn(move || {
            if let Err(Stalled) = fetch_head(&session, &live, kind, &fill) {
                // The player's own request then goes upstream, on a new client.
                server.redial(&session, &live);
            }
            fill.end();
        });
        if spawned.is_err() {
            // Nobody would ever fill it, and a request would wait for it.
            head.end();
        }
    }
}

/// A warm-up request timed out, or its connection failed.
struct Stalled;

/// Fetches the head of stream `kind` into `head`, as it arrives. A response
/// that isn't the bytes asked for leaves it without them (a 403 is left to
/// the player's own request, which resolves the stream again).
fn fetch_head(s: &Session, live: &Live, kind: &str, head: &Head) -> Result<(), Stalled> {
    let want = if kind == "v" { HEAD } else { AUDIO_HEAD };
    let range = format!("bytes=0-{}", want - 1);
    let Some(mut up) =
        fetch_upstream(live, &s.route, "GET", kind, Some(&range)).map_err(|_| Stalled)?
    else {
        return Ok(());
    };
    if up.status() != StatusCode::PARTIAL_CONTENT {
        return Ok(());
    }
    let content_type = up
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    if content_type.as_deref().is_some_and(is_playlist_type) {
        return Ok(());
    }
    let Some((0, last, total)) = content_range(&up) else {
        return Ok(());
    };
    if last >= want {
        return Ok(());
    }
    {
        let mut st = lock(&head.state);
        if st.ended {
            return Ok(());
        }
        st.meta = Some(Meta {
            last,
            total,
            content_type,
        });
        st.bytes.reserve_exact(last as usize + 1);
    }
    head.changed.notify_all();
    let want = last as usize + 1;
    let mut buf = vec![0u8; 16 << 10];
    let mut have = 0;
    while have < want {
        if s.cancelled.load(Ordering::Acquire) || head.ended() {
            return Ok(());
        }
        match up.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                let n = n.min(want - have);
                let mut st = lock(&head.state);
                if st.ended {
                    return Ok(());
                }
                st.bytes.extend_from_slice(&buf[..n]);
                have += n;
                drop(st);
                head.changed.notify_all();
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Stalled),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::server::relay::SEEK_CHUNK;
    use crate::server::testkit::*;
    use crate::store::testing::TempDir;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

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
    fn the_head_reaches_the_player_as_it_arrives_not_once_it_is_all_in() {
        let up = Upstream::start();
        // A round trip of 300 ms and then about 400 KB/s, as over Tor: the
        // whole video head takes more than two seconds to come.
        let (s, base, _d, _bin) =
            warm_server(&up, &format!("/big?size={SIZE}&ttfb=300&pace=40&run="));
        let info = s.open(&test_video()).unwrap();
        let (head, took, mut body, mut c) = first_byte(&base, rel(&base, &info.video), "bytes=0-");
        eprintln!("first byte of the video after {took:?}");
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert!(
            head.contains(&format!("Content-Range: bytes 0-{}/{SIZE}", SIZE - 1)),
            "{head}"
        );
        assert!(took < Duration::from_millis(900), "{took:?}");
        read_up_to(&mut c, &mut body, 200_000);
        assert!(body[..200_000] == big_bytes(0, 200_000), "the bytes differ");
        let (_, took, _, _a) = first_byte(
            &base,
            rel(&base, info.audio.as_deref().unwrap()),
            "bytes=0-",
        );
        eprintln!("first byte of the audio after {took:?}");
        assert!(took < Duration::from_millis(900), "{took:?}");
        // Both came from the requests made at open, and nothing else was
        // asked for meanwhile: no second request for the start, and no tails.
        let mut asked = ranges(&up);
        asked.sort();
        assert_eq!(
            asked,
            [
                format!("bytes=0-{}", HEAD - 1),
                format!("bytes=0-{}", AUDIO_HEAD - 1)
            ]
        );
    }

    #[test]
    fn what_follows_the_head_goes_over_the_heads_connection() {
        let up = KeepAlive::start(Duration::from_millis(400));
        let bin = TempDir::new();
        let ytdlp = fake_ytdlp(&bin.0, &up.url(&format!("/big?size={SIZE}&pace=2&run=")));
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        s.warming.store(true, Ordering::Relaxed);
        let info = s.open(&test_video()).unwrap();
        let (_, _, mut body, mut c) = first_byte(&base, rel(&base, &info.video), "bytes=0-");
        read_up_to(&mut c, &mut body, HEAD as usize + 100_000);
        assert!(body[..HEAD as usize + 100_000] == big_bytes(0, HEAD + 100_000));
        assert_eq!(
            up.ranges()[2..],
            [format!("bytes={HEAD}-{}", SIZE - 1)],
            "{:?}",
            up.ranges()
        );
        assert_eq!(up.connections(), 2, "a new connection was made");
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
        // asks for them when it opens an MP4. Only the end goes upstream.
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
        let mut asked = ranges(&up);
        asked.sort();
        assert_eq!(
            asked,
            [
                format!("bytes=0-{}", HEAD - 1),
                format!("bytes=0-{}", AUDIO_HEAD - 1),
                format!("bytes={tail}-{}", tail + SEEK_CHUNK - 1),
            ],
            "the player's requests for the start went upstream"
        );

        // Playing on from the start takes the head from memory and fetches
        // what follows it straight away.
        let (status, _, body) = get(video, "bytes=0-");
        assert_eq!(status, 206);
        assert!(body == big_bytes(0, SIZE), "the bytes differ");
        assert_eq!(ranges(&up)[3..], [format!("bytes={HEAD}-{}", SIZE - 1)]);
    }

    #[test]
    fn a_head_that_does_not_start_is_not_waited_for_long() {
        let up = Upstream::start();
        // The request for the video head gets no answer at all.
        let (s, base, _d, _bin) =
            warm_server(&up, &format!("/big?size={SIZE}&hold={}&run=", HEAD - 1));
        let info = s.open(&test_video()).unwrap();
        let started = Instant::now();
        let (status, _, body) = fetch(
            &base,
            "GET",
            rel(&base, &info.video),
            &[("Range", "bytes=0-99")],
        );
        assert_eq!((status, body), (206, big_bytes(0, 100)));
        let took = started.elapsed();
        assert!(took < HEAD_WAIT + Duration::from_secs(1), "{took:?}");
        assert!(ranges(&up).contains(&"bytes=0-99".to_string()));
    }

    #[test]
    fn a_head_that_stops_short_is_carried_on_from_where_it_stopped() {
        let up = Upstream::start();
        // The video head is cut off half-way.
        let (s, base, _d, _bin) = warm_server(
            &up,
            &format!("/big?size={SIZE}&pace=5&cut={}&run=", HEAD - 1),
        );
        let info = s.open(&test_video()).unwrap();
        let (status, _, body) = fetch(
            &base,
            "GET",
            rel(&base, &info.video),
            &[("Range", "bytes=0-1999999")],
        );
        assert_eq!(status, 206);
        assert!(body == big_bytes(0, 2_000_000), "{} bytes", body.len());
        let half = HEAD / 2;
        assert!(
            ranges(&up).contains(&format!("bytes={half}-1999999")),
            "{:?}",
            ranges(&up)
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
        assert_eq!(settled(&up), 2, "more was asked for after the switch");
        assert!(held.warm.find(&held, "v", 0, 0).is_none());
        assert!(lock(&held.warm.state).stopped);
    }
}
