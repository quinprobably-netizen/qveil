//! `GET api/wait?v=<id>`: the waiting room of an announced stream, until it
//! starts.
//!
//! The window opens it when a play says `waiting`, and the lines it reads are
//! NDJSON: `{"state":"upcoming","starts":<Unix seconds or null>}` when the
//! first lookup says when the stream is announced for (and again if that
//! changes), then `{"state":"now"}` once a lookup finds it started (the window
//! then plays it, once), or `{"state":"gave_up"}` six hours after the
//! announced time, with the engine's words for it (`errors::GAVE_UP`) and
//! their code. A lookup that fails, or a route that is left, ends it with the
//! usual error line.
//!
//! The lookups follow `next_ask`'s schedule, each over the route in use, on
//! an identity of its own (see `Runtime::pinned_ytdlp`), and stop with the
//! window: a client that hangs up is noticed within a fraction of a second,
//! also while the job is asleep between two lookups.

use super::Server;
use super::http::{NDJSON_HEAD, Request, Watch};
use crate::errors::{self, GAVE_UP};
use crate::ids::{self, VideoRef};
use crate::inflight::ROUTE_CHANGED;
use crate::live::Opening;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    let late = starts.map_or(0, |s| now - s);
    if late >= GIVE_UP {
        return None;
    }
    let due = match asked_at {
        None => now,
        Some(at) => at + if late < LATE { EVERY } else { EVERY_WHEN_LATE },
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

    /// Sleeps until `at`, or until `wanted` says no, which it is asked every
    /// tenth of a second.
    fn sleep_until(&self, at: i64, wanted: &dyn Fn() -> bool) {
        while wanted() {
            let left = self.until(at);
            if left.is_zero() {
                return;
            }
            std::thread::sleep(left.min(LOOK_EVERY));
        }
    }
}

/// The reply, a line at a time.
struct Lines<'a> {
    out: &'a mut TcpStream,
    started: bool,
}

impl Lines<'_> {
    fn say(&mut self, line: &Value) -> Result<()> {
        let mut text = String::new();
        if !self.started {
            text.push_str(NDJSON_HEAD);
        }
        text.push_str(&format!("{line}\n"));
        self.out.write_all(text.as_bytes())?;
        self.started = true;
        Ok(())
    }
}

/// What the window is told when the stream never started: the engine's words
/// for it, and their code, next to the state.
fn gave_up() -> Value {
    let mut line = errors::body(GAVE_UP);
    line["state"] = json!("gave_up");
    line
}

impl Server {
    /// `GET api/wait?v=<id>`: see the module's docs.
    pub(super) fn wait(self: &Arc<Self>, out: &mut TcpStream, req: &Request) -> Result<()> {
        let watch = Watch::new(out);
        let mut lines = Lines {
            out,
            started: false,
        };
        let waited = req.param("v").and_then(|target| {
            let v = ids::parse_video(target)
                .with_context(|| format!("{target:?} isn't a YouTube video"))?;
            self.wait_for_start(&v, &watch, &mut lines)
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
    /// left) any more.
    fn wait_for_start(&self, v: &VideoRef, watch: &Watch, lines: &mut Lines) -> Result<()> {
        let rt = self.runtime();
        let clock = Clock::start(rt.wait_speed);
        let began = clock.now();
        let wanted = || !watch.gone() && self.still_on(&rt);
        let mut starts = None;
        let mut asked = None;
        // What the window was last told the stream starts at.
        let mut said: Option<Option<i64>> = None;
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
            let Some(at) = next_ask(now, starts.or(Some(began)), asked) else {
                return lines.say(&gave_up());
            };
            if at > now {
                clock.sleep_until(at, &wanted);
                // What was asked of it may have changed while it slept, or the
                // time to give up have come.
                continue;
            }
            asked = Some(now);
            let found = rt.pinned_ytdlp().open_until(
                &v.watch_url(),
                &rt.cfg.format(),
                rt.cfg.max_height,
                &|| !wanted(),
            );
            if watch.gone() {
                return Ok(());
            }
            match self.guarded(&rt, found)? {
                Opening::Upcoming { starts: when, .. } => {
                    starts = when;
                    if said != Some(when) {
                        lines.say(&json!({ "state": "upcoming", "starts": when }))?;
                        said = Some(when);
                    }
                }
                // Started: a running stream, a finished one, or a video.
                Opening::Live(_) | Opening::Plain(_) => {
                    return lines.say(&json!({ "state": "now" }));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app;
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
    }

    /// The lines of a wait that runs to its end.
    fn waited(base: &str, query: &str) -> Vec<Value> {
        let mut c = TcpStream::connect(addr(base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            c,
            "GET {} HTTP/1.1\r\nHost: x\r\n\r\n",
            path(base, &format!("api/wait?{query}"))
        )
        .unwrap();
        let mut all = String::new();
        c.read_to_string(&mut all).unwrap();
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
            let mut c = TcpStream::connect(addr(base)).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            write!(
                c,
                "GET {} HTTP/1.1\r\nHost: x\r\n\r\n",
                path(base, &format!("api/wait?{query}"))
            )
            .unwrap();
            let mut r = BufReader::new(c);
            let mut head = String::new();
            while head.trim_end() != "" || head.is_empty() {
                head.clear();
                r.read_line(&mut head).unwrap();
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
        let up = LiveUpstream::start(60_000, 0, 20);
        let bin = TempDir::new();
        let ytdlp = fake_live_ytdlp_upcoming(&bin.0, &up, k, starts)
            .display()
            .to_string();
        let (s, base, d) = server_tuned(
            Config {
                ytdlp,
                ..Config::default()
            },
            |rt| rt.wait_speed = SPEED,
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
        // Three asks a minute apart: a minute is 50 ms here. Each is the
        // usual lookup and then the live one.
        assert_eq!(runs(&bin.0), 6);
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn waiting_asks_at_the_announced_time_and_not_before() {
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
        assert_eq!(runs(&bin.0), 4);
        assert!(
            began.elapsed() >= Duration::from_millis(900),
            "{:?}",
            began.elapsed()
        );
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
        let asked = runs(&bin.0) / 2;
        assert!((1..=2).contains(&asked), "{asked} asks");
    }

    #[test]
    fn waiting_stops_asking_when_the_window_hangs_up() {
        let (_s, base, _d, bin, _up) = announced(100_000, app::now() - 720);
        let mut open = Open::new(&base, &format!("v={ID}"));
        assert_eq!(open.line()["state"], "upcoming");
        // It is asking every 50 ms.
        wait_for("three asks", Duration::from_secs(5), || runs(&bin.0) >= 6);
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
