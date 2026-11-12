//! `GET api/wait?v=<id>[&starts=<ts>]`: the waiting room of an announced
//! stream, until it starts.
//!
//! The window opens it when a play says `waiting`, and passes the time that
//! answer gave (`starts`, Unix seconds), so that the first lookup is at that
//! time and not before. The lines it reads are NDJSON:
//!
//! - `{"state":"upcoming","starts":<Unix seconds or null>}`: at once if
//!   `starts` was given, otherwise when the first lookup says when the stream
//!   is announced for, and again whenever that changes. Also as a heartbeat,
//!   whenever nothing else has been said for a while (see
//!   `Runtime::wait_heartbeat`), so that the connection is never idle for
//!   long: it is sent while the job sleeps, and while a lookup runs.
//! - `{"state":"now"}` once a lookup finds the stream started (the window
//!   then plays it, once, and that play opens it for real). The job ends.
//! - `{"state":"gave_up", ...}` six hours after the announced time, with the
//!   engine's words for it (`errors::GAVE_UP`) and their code. The job ends.
//! - The usual error line, for something that is not a video, for a route
//!   that was left, and for a lookup that no later one can answer otherwise
//!   (the stream was made private or members-only, is age-restricted or not
//!   offered in the route's region, was taken down, or ended without a
//!   recording: see `errors::for_good`). The job ends.
//!
//! The lookups follow `next_ask`'s schedule: one live lookup each, over the
//! route in use, on an identity of its own (see `Runtime::pinned_ytdlp`). Any
//! other that fails (a Tor circuit that stalled, a bot check, a timeout) says
//! nothing and is made up for by the next. The job stops with the window: a
//! client that hangs up is noticed within a fraction of a second, also while
//! it is asleep between two lookups or in the middle of one.

use super::Server;
use super::http::{NDJSON_HEAD, Request, Watch};
use crate::errors::{self, GAVE_UP};
use crate::ids::{self, VideoRef};
use crate::inflight::ROUTE_CHANGED;
use crate::live::Opening;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long after one ask the next follows (seconds).
const EVERY: i64 = 60;
/// ... once the stream is `LATE` late.
const EVERY_WHEN_LATE: i64 = 5 * 60;
const LATE: i64 = 30 * 60;
/// How late the stream is when it is given up on.
const GIVE_UP: i64 = 6 * 60 * 60;
/// How often a sleeping job looks at whether it is still wanted.
const LOOK_EVERY: Duration = Duration::from_millis(100);

/// When the next lookup is due (Unix seconds), or `None` once the stream is
/// given up on. `starts` is when it was announced for, if that is known, and
/// `asked_at` when the last lookup was made. The first is at the announced
/// time, or at once if that is unknown or past; the ones after it a minute
/// apart, and 5 minutes apart once the stream is 30 minutes late. A lookup
/// made before the announced time (to learn it) doesn't bring the next one
/// forward: there is none before it. Without an announced time there is no
/// telling how late the stream is, so it is never given up on here.
pub(super) fn next_ask(now: i64, starts: Option<i64>, asked_at: Option<i64>) -> Option<i64> {
    let late = starts.map_or(0, |s| now.saturating_sub(s));
    if late >= GIVE_UP {
        return None;
    }
    let due = match asked_at {
        None => now,
        Some(at) => at.saturating_add(if late < LATE { EVERY } else { EVERY_WHEN_LATE }),
    };
    Some(starts.map_or(due, |s| due.max(s)))
}

/// What time it is for the waiting room: Unix seconds, running `speed` times
/// as fast as the clock (1, except in tests, which shrink the minute: see
/// `Runtime::wait_speed`). It goes by the wall clock, so a computer that was
/// asleep finds the time that passed.
struct Clock {
    origin: SystemTime,
    origin_ms: i64,
    speed: i64,
}

impl Clock {
    fn start(speed: u32) -> Self {
        let origin = SystemTime::now();
        let origin_ms = origin
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        Clock {
            origin,
            origin_ms,
            speed: i64::from(speed.max(1)),
        }
    }

    fn now(&self) -> i64 {
        let passed = self.origin.elapsed().map_or(0, |d| d.as_millis() as i64);
        (self.origin_ms + passed * self.speed).div_euclid(1000)
    }

    /// How long, on the clock's own time, until `at`.
    fn until(&self, at: i64) -> Duration {
        let secs = at.saturating_sub(self.now());
        if secs <= 0 {
            return Duration::ZERO;
        }
        Duration::from_millis((secs.unsigned_abs() * 1000 / self.speed.unsigned_abs()).max(1))
    }
}

/// The reply, a line at a time. A line can be written from where a lookup is
/// waited for (the heartbeat), so what writing changes is behind cells.
struct Lines<'a> {
    out: RefCell<&'a mut TcpStream>,
    started: Cell<bool>,
    /// When the last line was written (or the reply began).
    last: Cell<Instant>,
    heartbeat: Duration,
}

impl<'a> Lines<'a> {
    fn new(out: &'a mut TcpStream, heartbeat: Duration) -> Self {
        Lines {
            out: RefCell::new(out),
            started: Cell::new(false),
            last: Cell::new(Instant::now()),
            heartbeat,
        }
    }

    fn say(&self, line: &Value) -> Result<()> {
        let mut text = String::new();
        if !self.started.get() {
            text.push_str(NDJSON_HEAD);
        }
        text.push_str(&format!("{line}\n"));
        self.out.borrow_mut().write_all(text.as_bytes())?;
        self.started.set(true);
        self.last.set(Instant::now());
        Ok(())
    }

    /// Says that the stream is still announced for `starts`, if nothing has
    /// been said for a heartbeat.
    fn beat(&self, starts: Option<i64>) -> Result<()> {
        if self.last.get().elapsed() >= self.heartbeat {
            self.say(&upcoming(starts))?;
        }
        Ok(())
    }
}

fn upcoming(starts: Option<i64>) -> Value {
    json!({ "state": "upcoming", "starts": starts })
}

/// What the window is told when the stream never started: the engine's words
/// for it, and their code, next to the state.
fn gave_up() -> Value {
    let mut line = errors::body(GAVE_UP);
    line["state"] = json!("gave_up");
    line
}

impl Server {
    /// `GET api/wait?v=<id>[&starts=<ts>]`: see the module's docs. A `starts`
    /// that isn't a number is taken as not given.
    pub(super) fn wait(self: &Arc<Self>, out: &mut TcpStream, req: &Request) -> Result<()> {
        let watch = Watch::new(out);
        let lines = Lines::new(out, self.runtime().wait_heartbeat);
        let announced = req.query.get("starts").and_then(|s| s.parse().ok());
        let waited = req.param("v").and_then(|target| {
            let v = ids::parse_video(target)
                .with_context(|| format!("{target:?} isn't a YouTube video"))?;
            self.wait_for_start(&v, announced, &watch, &lines)
        });
        match waited {
            Ok(()) => Ok(()),
            // Nobody is listening for the reason.
            Err(_) if watch.gone() => Ok(()),
            Err(e) => lines.say(&errors::body(&format!("{e:#}"))),
        }
    }

    /// Asks about `v` on `next_ask`'s schedule until it has started, the
    /// time to give up has come, or nobody is waiting (or the route was
    /// left) any more. `announced` is when it is known to start, if it is.
    fn wait_for_start(
        &self,
        v: &VideoRef,
        announced: Option<i64>,
        watch: &Watch,
        lines: &Lines,
    ) -> Result<()> {
        let rt = self.runtime();
        let clock = Clock::start(rt.wait_speed);
        let began = clock.now();
        let starts = Cell::new(announced);
        // What the window was last told the stream starts at.
        let mut said = None;
        if announced.is_some() {
            lines.say(&upcoming(announced))?;
            said = Some(announced);
        }
        // Whether a heartbeat could not be written: nobody is reading.
        let deaf = Cell::new(false);
        // Asked while a lookup runs: whether to end it.
        let abort = || {
            if watch.gone() || !self.still_on(&rt) {
                return true;
            }
            if lines.beat(starts.get()).is_err() {
                deaf.set(true);
            }
            deaf.get()
        };
        let mut asked = None;
        loop {
            if watch.gone() {
                return Ok(());
            }
            if !self.still_on(&rt) {
                bail!(ROUTE_CHANGED);
            }
            let now = clock.now();
            // How late it is is measured from when the wait began if no time
            // was announced: a stream that never says is not waited for forever.
            let Some(at) = next_ask(now, starts.get().or(Some(began)), asked) else {
                return lines.say(&gave_up());
            };
            if at > now {
                std::thread::sleep(clock.until(at).min(LOOK_EVERY));
                lines.beat(starts.get())?;
                // What was asked of it may have changed while it slept, or the
                // time to give up have come.
                continue;
            }
            asked = Some(now);
            let found = rt
                .pinned_ytdlp()
                .live_until(&v.watch_url(), rt.cfg.max_height, &abort);
            if watch.gone() || deaf.get() {
                return Ok(());
            }
            if !self.still_on(&rt) {
                bail!(ROUTE_CHANGED);
            }
            match found {
                Ok(Opening::Upcoming { starts: when, .. }) => {
                    starts.set(when);
                    if said != Some(when) {
                        lines.say(&upcoming(when))?;
                        said = Some(when);
                    }
                }
                // Started (a live lookup finds nothing else, but a video that
                // is no stream any more has started too).
                Ok(Opening::Live(_) | Opening::Plain(_)) => {
                    return lines.say(&json!({ "state": "now" }));
                }
                // Private, members-only, out of region, age-restricted,
                // taken down: no ask after this one will find it started.
                Err(e) if errors::for_good(&format!("{e:#}")) => return Err(e),
                // A Tor circuit that stalled, a bot check, a timeout: the
                // schedule goes on, and the next ask is another try.
                Err(e) => note!("qveil: asking whether a stream has started failed: {e:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app;
    use crate::app::Runtime;
    use crate::config::Config;
    use crate::errors::GAVE_UP;
    use crate::server::testkit::*;
    use crate::store::testing::TempDir;
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::time::{Duration, Instant};

    const ID: &str = "dQw4w9WgXcQ";
    /// What the minute is shrunk to: 50 ms.
    const SPEED: u32 = 1200;

    #[test]
    fn next_ask_follows_the_schedule() {
        let starts = Some(1000);
        // (now, asked at, the next ask)
        for (now, asked, next) in [
            // At the announced time, not before.
            (500, None, Some(1000)),
            (500, Some(450), Some(1000)),
            // Then every minute.
            (1000, Some(1000), Some(1060)),
            (1500, Some(1440), Some(1500)),
            (2799, Some(2740), Some(2800)),
            // Every 5 minutes once 30 minutes late.
            (2900, Some(2860), Some(3160)),
            // Late already, and never asked: now.
            (1700, None, Some(1700)),
            // Six hours late: never.
            (22599, Some(22500), Some(22800)),
            (22600, Some(22500), None),
            (30000, None, None),
        ] {
            assert_eq!(
                next_ask(now, starts, asked),
                next,
                "now {now}, asked {asked:?}"
            );
        }
        // Without an announced time there is no telling how late it is: a
        // first ask at once, then every minute.
        assert_eq!(next_ask(700, None, None), Some(700));
        assert_eq!(next_ask(700, None, Some(690)), Some(750));
        // A time that is far off the scale is no reason to overflow.
        assert_eq!(next_ask(700, Some(i64::MIN), Some(690)), None);
        assert_eq!(next_ask(700, Some(i64::MAX), None), Some(i64::MAX));
    }

    fn request(base: &str, query: &str) -> TcpStream {
        let mut c = TcpStream::connect(addr(base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            c,
            "GET {} HTTP/1.1\r\nHost: x\r\n\r\n",
            path(base, &format!("api/wait?{query}"))
        )
        .unwrap();
        c
    }

    /// The lines of a wait that runs to its end.
    fn waited(base: &str, query: &str) -> Vec<Value> {
        let mut all = String::new();
        request(base, query).read_to_string(&mut all).unwrap();
        let (head, body) = all.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("application/x-ndjson"), "{head}");
        body.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// A wait that is still going: the response head is read, and each line
    /// is read as it comes.
    struct Open(BufReader<TcpStream>);

    impl Open {
        fn new(base: &str, query: &str) -> Self {
            let mut r = BufReader::new(request(base, query));
            let mut head = String::new();
            loop {
                head.clear();
                let n = r.read_line(&mut head).unwrap();
                assert!(n > 0, "the reply ended inside its head");
                if head == "\r\n" {
                    break;
                }
            }
            Open(r)
        }

        fn line(&mut self) -> Value {
            let mut line = String::new();
            self.0.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
        }
    }

    /// A server whose yt-dlp says that the stream starts at `starts` for
    /// `k` live lookups, and then that it is live, with the waiting room's
    /// minute shrunk to 50 ms.
    fn announced(k: usize, starts: i64) -> (Arc<Server>, String, TempDir, TempDir, LiveUpstream) {
        announced_tuned(k, starts, |_| {})
    }

    /// `announced`, with `tune` changing the runtime too.
    fn announced_tuned(
        k: usize,
        starts: i64,
        tune: impl FnOnce(&mut Runtime),
    ) -> (Arc<Server>, String, TempDir, TempDir, LiveUpstream) {
        let up = LiveUpstream::start(60_000, 0, 20);
        let bin = TempDir::new();
        let ytdlp = fake_live_ytdlp_upcoming(&bin.0, &up, k, starts)
            .display()
            .to_string();
        serve(ytdlp, tune, bin, up)
    }

    fn serve(
        ytdlp: String,
        tune: impl FnOnce(&mut Runtime),
        bin: TempDir,
        up: LiveUpstream,
    ) -> (Arc<Server>, String, TempDir, TempDir, LiveUpstream) {
        let (s, base, d) = server_tuned(
            Config {
                ytdlp,
                ..Config::default()
            },
            |rt| {
                rt.wait_speed = SPEED;
                tune(rt);
            },
        );
        (s, base, d, bin, up)
    }

    #[test]
    fn waiting_says_now_once_the_stream_starts() {
        let starts = app::now() - 720;
        let (_s, base, _d, bin, _up) = announced(2, starts);
        let began = Instant::now();
        let lines = waited(&base, &format!("v={ID}"));
        assert_eq!(
            lines,
            [
                json!({"state":"upcoming","starts":starts}),
                json!({"state":"now"})
            ]
        );
        // Three asks a minute apart: a minute is 50 ms here.
        assert_eq!(runs(&bin.0), 3);
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn each_ask_is_one_lookup() {
        let starts = app::now() - 720;
        let (_s, base, _d, bin, _up) = announced(2, starts);
        waited(&base, &format!("v={ID}&starts={starts}"));
        // Three asks, one yt-dlp call each, and each is the live lookup:
        // not the usual one first, which a stream that is known to be one
        // doesn't need.
        assert_eq!(runs(&bin.0), 3);
        for n in 1..=3 {
            let conf = std::fs::read_to_string(bin.0.join(format!("conf{n}"))).unwrap();
            assert!(
                conf.lines().any(|l| l == "--live-from-start"),
                "{n}: {conf}"
            );
        }
    }

    #[test]
    fn waiting_without_a_known_start_asks_at_once_and_then_at_the_start() {
        // 20 minutes ahead is a second here.
        let starts = app::now() + 1200;
        let (_s, base, _d, bin, _up) = announced(1, starts);
        let began = Instant::now();
        let lines = waited(&base, &format!("v={ID}"));
        assert_eq!(
            lines,
            [
                json!({"state":"upcoming","starts":starts}),
                json!({"state":"now"})
            ]
        );
        // The first ask learns when it starts; the second is at that time,
        // not a minute (50 ms) later.
        assert_eq!(runs(&bin.0), 2);
        assert!(
            began.elapsed() >= Duration::from_millis(900),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn waiting_with_a_known_start_asks_first_at_the_start() {
        // 20 minutes ahead is a second here.
        let starts = app::now() + 1200;
        let (_s, base, _d, bin, _up) = announced(0, starts);
        let mut open = Open::new(&base, &format!("v={ID}&starts={starts}"));
        // Said at once, before anyone has asked.
        assert_eq!(open.line(), json!({"state":"upcoming","starts":starts}));
        assert_eq!(runs(&bin.0), 0);
        let began = Instant::now();
        assert_eq!(open.line(), json!({"state":"now"}));
        // The only ask is at the start.
        assert_eq!(runs(&bin.0), 1);
        assert!(
            began.elapsed() >= Duration::from_millis(800),
            "{:?}",
            began.elapsed()
        );

        // A start that is past is asked at once, and a start that is no
        // number is taken as none: both ask at once too.
        for query in [
            format!("&starts={}", app::now() - 720),
            "&starts=soon".to_string(),
        ] {
            let before = runs(&bin.0);
            let began = Instant::now();
            let lines = waited(&base, &format!("v={ID}{query}"));
            assert_eq!(lines.last(), Some(&json!({"state":"now"})), "{query}");
            assert_eq!(runs(&bin.0), before + 1, "{query}");
            assert!(began.elapsed() < Duration::from_millis(700), "{query}");
        }
    }

    #[test]
    fn a_failed_ask_is_tried_again_at_the_next_one() {
        let up = LiveUpstream::start(60_000, 0, 20);
        let bin = TempDir::new();
        let fake = fake_live_ytdlp_upcoming(&bin.0, &up, 1, app::now() - 720);
        // The first two asks fail, as a bot check or a limit on how fast
        // does; then it is the fake, which announces once and starts.
        let wrapper = TempDir::new();
        let ytdlp = crate::store::testing::script(
            &wrapper.0.join("yt-dlp"),
            &format!(
                r#"d='{}'
n=$(cat "$d/failed" 2>/dev/null || echo 0)
if [ "$n" -lt 2 ]; then
  echo $((n + 1)) > "$d/failed"
  if [ "$n" -eq 0 ]; then
    echo 'ERROR: [youtube] x: Sign in to confirm you are not a bot' >&2
  else
    echo 'ERROR: [youtube] x: HTTP Error 429: Too Many Requests' >&2
  fi
  exit 1
fi
exec '{}' "$@"
"#,
                wrapper.0.display(),
                fake.display()
            ),
        );
        let (_s, base, _d, _bin, _up) = serve(ytdlp, |_| {}, bin, up);
        let began = Instant::now();
        let lines = waited(&base, &format!("v={ID}"));
        // Nothing was said of the failures.
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0]["state"], "upcoming");
        assert_eq!(lines[1], json!({"state":"now"}));
        assert_eq!(
            std::fs::read_to_string(wrapper.0.join("failed"))
                .unwrap()
                .trim(),
            "2"
        );
        // A minute (50 ms) between asks: four of them.
        assert!(
            began.elapsed() >= Duration::from_millis(120),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_stream_that_no_ask_can_find_started_ends_the_wait_with_why() {
        let bin = TempDir::new();
        let ytdlp = crate::store::testing::script(
            &bin.0.join("yt-dlp"),
            &format!(
                r#"d='{}'
n=$(cat "$d/runs" 2>/dev/null || echo 0)
echo $((n + 1)) > "$d/runs"
cat "$d/says" >&2
exit 1
"#,
                bin.0.display()
            ),
        );
        let up = LiveUpstream::start(60_000, 0, 20);
        let home = TempDir::new();
        let (_s, base, _d, _bin, _up) = serve(ytdlp, |_| {}, home, up);
        for (says, code) in [
            (
                "Private video. Sign in if you've been granted access to this video",
                "private",
            ),
            (
                "Join this channel to get access to members-only content like this video",
                "members_only",
            ),
            (
                "The uploader has not made this video available in your country",
                "region",
            ),
            (
                "Sign in to confirm your age. This video may be inappropriate for some users.",
                "age_restricted",
            ),
            (
                "Video unavailable. This video has been removed by the uploader",
                "other",
            ),
            (
                "This live stream recording is not available.",
                "no_recording",
            ),
        ] {
            std::fs::write(bin.0.join("says"), format!("ERROR: [youtube] x: {says}\n")).unwrap();
            let before = runs(&bin.0);
            let lines = waited(&base, &format!("v={ID}"));
            // One ask, and its answer is the last word: no "checking every
            // minute" for six hours.
            assert_eq!(lines.len(), 1, "{says}: {lines:?}");
            assert_eq!(lines[0]["code"], code, "{says}: {lines:?}");
            assert!(lines[0]["error"].as_str().is_some(), "{says}");
            assert!(lines[0].get("state").is_none(), "{says}: {lines:?}");
            assert_eq!(runs(&bin.0), before + 1, "{says}");
        }
    }

    #[test]
    fn waiting_gives_up_six_hours_after_the_announced_time() {
        // Ten minutes short of six hours late.
        let starts = app::now() - 21_000;
        let (_s, base, _d, bin, _up) = announced(10_000, starts);
        let lines = waited(&base, &format!("v={ID}"));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0], json!({"state":"upcoming","starts":starts}));
        assert_eq!(lines[1]["state"], "gave_up");
        assert_eq!(lines[1]["code"], "gave_up");
        assert_eq!(lines[1]["error"], GAVE_UP);
        // Late by more than 30 minutes, so asked every 5, not every minute:
        // at most the first ask and one after it before the end.
        let asked = runs(&bin.0);
        assert!((1..=2).contains(&asked), "{asked} asks");
    }

    #[test]
    fn waiting_stops_asking_when_the_window_hangs_up() {
        let (_s, base, _d, bin, _up) = announced(100_000, app::now() - 720);
        let mut open = Open::new(&base, &format!("v={ID}"));
        assert_eq!(open.line()["state"], "upcoming");
        // It is asking every 50 ms.
        wait_for("three asks", Duration::from_secs(5), || runs(&bin.0) >= 3);
        drop(open);
        // The ask that was going finishes; none follows it.
        std::thread::sleep(Duration::from_millis(400));
        let after = runs(&bin.0);
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(runs(&bin.0), after, "it carried on asking");
    }

    #[test]
    fn waiting_ends_with_an_error_when_the_route_changes() {
        let (_s, base, _d, bin, _up) = announced(100_000, app::now() - 720);
        let mut open = Open::new(&base, &format!("v={ID}"));
        assert_eq!(open.line()["state"], "upcoming");
        switch_to_tor(&base);
        let said = open.line();
        assert_eq!(said["code"], "route_changed", "{said}");
        // Nothing goes out on the old route, nor on Tor, for it.
        std::thread::sleep(Duration::from_millis(300));
        let after = runs(&bin.0);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(runs(&bin.0), after);
    }

    #[test]
    fn waiting_sends_a_heartbeat() {
        // Far ahead (a quarter of an hour here), so that nothing is asked.
        let starts = app::now() + 1_000_000;
        let (_s, base, _d, bin, _up) = announced_tuned(100_000, starts, |rt| {
            rt.wait_heartbeat = Duration::from_millis(100);
        });
        let mut open = Open::new(&base, &format!("v={ID}&starts={starts}"));
        let began = Instant::now();
        for n in 0..4 {
            assert_eq!(
                open.line(),
                json!({"state":"upcoming","starts":starts}),
                "line {n}"
            );
        }
        // One at once, and three more a heartbeat apart.
        let took = began.elapsed();
        assert!(took >= Duration::from_millis(250), "{took:?}");
        assert!(took < Duration::from_secs(3), "{took:?}");
        assert_eq!(runs(&bin.0), 0);
    }

    #[test]
    fn the_heartbeat_goes_on_while_a_lookup_runs() {
        let up = LiveUpstream::start(60_000, 0, 20);
        let bin = TempDir::new();
        let ytdlp = hanging_ytdlp(&bin.0);
        let (_s, base, _d, bin, _up) = serve(
            ytdlp,
            |rt| rt.wait_heartbeat = Duration::from_millis(100),
            bin,
            up,
        );
        // Nothing is known, and the first lookup hangs: it is all heartbeat.
        let mut open = Open::new(&base, &format!("v={ID}"));
        let began = Instant::now();
        for n in 0..3 {
            assert_eq!(
                open.line(),
                json!({"state":"upcoming","starts":null}),
                "{n}"
            );
        }
        assert!(began.elapsed() >= Duration::from_millis(150));
        let pid = std::fs::read_to_string(bin.0.join("pid")).unwrap();
        let alive = || std::path::Path::new(&format!("/proc/{}", pid.trim())).exists();
        assert!(alive(), "the lookup was not running");
        // The window leaves, and the lookup it was waiting for is ended.
        drop(open);
        wait_for("the lookup to end", Duration::from_secs(5), || !alive());
    }

    #[test]
    fn waiting_for_something_that_is_not_a_video_says_so() {
        let (_s, base, _d, bin, _up) = announced(1, 0);
        for query in ["v=nope", "v=https%3A%2F%2Fexample.com%2Fwatch", ""] {
            let lines = waited(&base, query);
            assert_eq!(lines.len(), 1, "{query}: {lines:?}");
            assert!(lines[0]["error"].as_str().is_some(), "{query}: {lines:?}");
            assert!(lines[0].get("state").is_none(), "{query}: {lines:?}");
        }
        assert_eq!(runs(&bin.0), 0);
    }
}
