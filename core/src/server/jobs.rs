//! The feed refresh, as a job that requests join.
//!
//! A refresh is a burst of requests for every channel, and over Tor it takes
//! minutes. It must not run twice at once (two windows, a button pressed
//! again, a window that was restarted), because the second burst would add
//! nothing but a second set of requests for exactly the same channels. So
//! there is one job at a time: a request for a refresh while one is running
//! joins it, sees its progress and gets its result, and doesn't start another.
//!
//! The job runs on a thread of its own, and so doesn't depend on any client:
//! when every one of them has hung up, it carries on, and saves what it
//! fetches, which the next request finds in the cache.

use super::Server;
use super::http::{NDJSON_HEAD, Request, Watch, respond};
use crate::app::{self, Refresh, Refreshed, Runtime};
use crate::errors;
use crate::net::Net;
use crate::util::lock;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How long a client waits for news before it looks whether the other end has
/// hung up. A hung-up client is noticed within this, and a job that is slow
/// to report doesn't leave a thread waiting for a client that left.
const POLL: Duration = Duration::from_millis(250);

/// The refresh that is running, if there is one.
#[derive(Default)]
pub(super) struct Jobs {
    running: Mutex<Option<Arc<FeedJob>>>,
}

pub(super) struct FeedJob {
    /// The route it runs on. A request after a route change must not join a
    /// job that is being cut off by it, and gets one of its own.
    net: Net,
    /// What it fetches, which is what a request must be covered by to join.
    refresh: Refresh,
    state: Mutex<JobState>,
    changed: Condvar,
}

#[derive(Clone, Default)]
struct JobState {
    /// Counts changes, so that a waiter can tell there is something new.
    version: u64,
    /// `(done, total)`, once the job knows what is due.
    progress: Option<(usize, usize)>,
    /// How it ended. The error is the text of what went wrong (not a request's
    /// own: each one that joined words it for itself).
    finished: Option<Result<Arc<Refreshed>, String>>,
}

impl FeedJob {
    fn new(net: Net, refresh: Refresh) -> Arc<Self> {
        Arc::new(FeedJob {
            net,
            refresh,
            state: Mutex::default(),
            changed: Condvar::new(),
        })
    }

    fn update(&self, change: impl FnOnce(&mut JobState)) {
        let mut state = lock(&self.state);
        change(&mut state);
        state.version += 1;
        drop(state);
        self.changed.notify_all();
    }

    fn progress(&self, done: usize, total: usize) {
        self.update(|s| s.progress = Some((done, total)));
    }

    /// Ends the job. The first ending counts.
    fn finish(&self, result: Result<Refreshed>) {
        self.update(|s| {
            if s.finished.is_none() {
                s.finished = Some(result.map(Arc::new).map_err(|e| format!("{e:#}")));
            }
        });
    }

    fn running(&self) -> bool {
        lock(&self.state).finished.is_none()
    }

    /// The state once it has changed since `seen`, or `POLL` has passed.
    fn wait(&self, seen: u64) -> JobState {
        let mut state = lock(&self.state);
        if state.version == seen {
            state = self
                .changed
                .wait_timeout(state, POLL)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        state.clone()
    }
}

impl FeedJob {
    /// Waits until the job is over, or until `gone` says nobody is waiting any
    /// more. Whether it is over.
    fn wait_over(&self, gone: impl Fn() -> bool) -> bool {
        let mut seen = 0;
        loop {
            let state = self.wait(seen);
            seen = state.version;
            if state.finished.is_some() {
                return true;
            }
            if gone() {
                return false;
            }
        }
    }
}

/// Ends the job if its thread does, however that happens (a panic, say), so
/// that nobody waits for a result that will never come.
struct EndWithThread(Arc<FeedJob>);

impl Drop for EndWithThread {
    fn drop(&mut self) {
        self.0
            .finish(Err(anyhow!("the refresh stopped unexpectedly")));
    }
}

/// What a request for a refresh gets from `Jobs::join_or_start`.
enum Claim {
    /// The job that does it: one that was running and covers it, or one that
    /// was just started.
    Job(Arc<FeedJob>),
    /// A job is running that doesn't cover it. Wait for that one to be over
    /// and ask again (the route may have changed by then, and with it the
    /// `Net` to ask with).
    Busy(Arc<FeedJob>),
}

impl Jobs {
    /// The job that is running on `net`'s route and covers `refresh`, or a
    /// new one, which `start` is given to run. The check and the start happen
    /// under one lock, so that two requests arriving together share one job.
    ///
    /// A running job that doesn't cover `refresh` is not joined, and nor is a
    /// second one started beside it: that is `Claim::Busy`. (A job on another
    /// route is being cut off, and doesn't count.)
    fn join_or_start(
        &self,
        net: &Net,
        refresh: &Refresh,
        start: impl FnOnce(Arc<FeedJob>),
    ) -> Claim {
        let mut running = lock(&self.running);
        if let Some(job) = running.as_ref()
            && job.running()
            && job.net.same_route(net)
        {
            return if job.refresh.covers(refresh) {
                Claim::Job(Arc::clone(job))
            } else {
                Claim::Busy(Arc::clone(job))
            };
        }
        let job = FeedJob::new(net.clone(), refresh.clone());
        *running = Some(Arc::clone(&job));
        start(Arc::clone(&job));
        Claim::Job(job)
    }
}

/// What a request wants of the feed it gets back.
#[derive(Clone, Copy)]
struct Shape {
    no_shorts: bool,
    limit: usize,
}

impl Server {
    /// `GET api/feed`: the feed, and with `refresh=1` first a refresh of the
    /// channels that are due (`&force=1` all of them, `&channel=UC…` one),
    /// which waits its turn if a refresh of something else is running.
    /// With `&stream=1` the reply is a stream of lines: `{"progress":
    /// {"done":N,"total":M}}` as channels finish, and last `{"result": …}`
    /// (the feed, as it is without `stream`) or `{"error": …, "code": …}`.
    pub(super) fn feed(self: &Arc<Self>, out: &mut TcpStream, req: &Request) -> Result<()> {
        let refresh = match req.query.get("channel").filter(|c| !c.is_empty()) {
            _ if !req.flag("refresh") => Refresh::Off,
            Some(id) => Refresh::Channel(id.clone()),
            None if req.flag("force") => Refresh::Force,
            None => Refresh::Stale,
        };
        let shape = Shape {
            no_shorts: !req.flag("shorts"),
            limit: req.num("n", 150),
        };
        let stream = req.flag("stream");
        if refresh == Refresh::Off {
            // Nothing to fetch, so nothing to wait for.
            let rt = self.runtime();
            let feed = app::feed(
                &rt,
                &self.paths,
                Refresh::Off,
                shape.no_shorts,
                shape.limit,
                &|| true,
            );
            return reply(out, stream, feed.map(|f| json!(f)));
        }
        let watch = Watch::new(out);
        let job = loop {
            // Taken again after a wait, for the route may have changed.
            let rt = self.runtime();
            let claim = self.jobs.join_or_start(&rt.net, &refresh, |job| {
                self.start_job(&rt, refresh.clone(), job)
            });
            match claim {
                Claim::Job(job) => break job,
                // A job that doesn't fetch what this wants: wait for it to be
                // over, unless the client hangs up first.
                Claim::Busy(busy) => {
                    if !busy.wait_over(|| watch.gone()) {
                        return Ok(());
                    }
                }
            }
        };
        if stream {
            self.stream_job(out, &job, shape)
        } else {
            self.wait_for_job(out, &job, shape)
        }
    }

    fn start_job(self: &Arc<Self>, rt: &Arc<Runtime>, refresh: Refresh, job: Arc<FeedJob>) {
        let (server, rt) = (Arc::clone(self), Arc::clone(rt));
        let thread_job = Arc::clone(&job);
        let spawned = std::thread::Builder::new()
            .name("feed-refresh".into())
            .spawn(move || {
                let _ends_with_the_thread = EndWithThread(Arc::clone(&thread_job));
                // A refresh that is running when the route changes stops
                // fetching (see `feed::fetch_all`).
                let current = || server.still_on(&rt);
                let done = app::refresh(&rt, &server.paths, refresh, &current, &|done, total| {
                    thread_job.progress(done, total)
                });
                thread_job.finish(done);
            });
        if let Err(e) = spawned {
            job.finish(Err(e.into()));
        }
    }

    /// The feed as the request wants it, from what the job left in the cache.
    fn job_result(&self, finished: Result<Arc<Refreshed>, String>, shape: Shape) -> Result<Value> {
        let done = finished.map_err(anyhow::Error::msg)?;
        Ok(json!(app::feed_result(
            &self.paths,
            shape.no_shorts,
            shape.limit,
            &done
        )?))
    }

    /// Answers with the feed once the job is over. Without `stream` there is
    /// nothing to send before that, but the client may hang up meanwhile.
    fn wait_for_job(&self, out: &mut TcpStream, job: &FeedJob, shape: Shape) -> Result<()> {
        let watch = Watch::new(out);
        let mut seen = 0;
        loop {
            let state = job.wait(seen);
            seen = state.version;
            if let Some(finished) = state.finished {
                return reply(out, false, self.job_result(finished, shape));
            }
            if watch.gone() {
                return Ok(());
            }
        }
    }

    /// Streams the job's progress, then its result.
    fn stream_job(&self, out: &mut TcpStream, job: &FeedJob, shape: Shape) -> Result<()> {
        if out.write_all(NDJSON_HEAD.as_bytes()).is_err() {
            return Ok(());
        }
        let watch = Watch::new(out);
        let (mut seen, mut told) = (0, None);
        loop {
            let state = job.wait(seen);
            seen = state.version;
            if let Some((done, total)) = state.progress.filter(|p| Some(*p) != told) {
                told = Some((done, total));
                let line = json!({ "progress": { "done": done, "total": total } });
                if write_line(out, &line).is_err() {
                    return Ok(());
                }
            }
            if let Some(finished) = state.finished {
                let line = match self.job_result(finished, shape) {
                    Ok(feed) => json!({ "result": feed }),
                    Err(e) => errors::body(&format!("{e:#}")),
                };
                let _ = write_line(out, &line);
                return Ok(());
            }
            if watch.gone() {
                return Ok(());
            }
        }
    }
}

fn write_line(out: &mut TcpStream, line: &Value) -> std::io::Result<()> {
    out.write_all(format!("{line}\n").as_bytes())
}

/// Answers with a feed that is ready, or says why it isn't: as the whole
/// reply, or as the one line of a stream.
fn reply(out: &mut TcpStream, stream: bool, feed: Result<Value>) -> Result<()> {
    match (stream, feed) {
        (false, Ok(feed)) => respond(out, 200, "application/json", feed.to_string().as_bytes()),
        (false, Err(e)) => {
            let body = errors::body(&format!("{e:#}")).to_string();
            respond(out, 500, "application/json", body.as_bytes())
        }
        (true, feed) => {
            let line = match feed {
                Ok(feed) => json!({ "result": feed }),
                Err(e) => errors::body(&format!("{e:#}")),
            };
            out.write_all(NDJSON_HEAD.as_bytes())?;
            write_line(out, &line)?;
            Ok(())
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
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

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

    /// The lines of a streamed reply (and its headers), once it has ended.
    fn lines(base: &str, query: &str) -> (std::collections::HashMap<String, String>, Vec<Value>) {
        let (status, headers, body) = fetch(base, "GET", &format!("api/feed?{query}"), &[]);
        assert_eq!(status, 200);
        let lines = String::from_utf8(body)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l:?}: {e}")))
            .collect();
        (headers, lines)
    }

    fn progress(lines: &[Value]) -> Vec<(u64, u64)> {
        lines
            .iter()
            .filter_map(|l| {
                let p = l.get("progress")?;
                Some((p["done"].as_u64()?, p["total"].as_u64()?))
            })
            .collect()
    }

    #[test]
    fn a_streamed_refresh_reports_each_channel_and_ends_with_the_feed() {
        use crate::store::testing::listing_runs as runs;
        let (_s, base, _d, bin) = feed_server("sleep 0.1", 3);
        let (headers, lines) = lines(&base, "refresh=1&stream=1");
        assert_eq!(headers["content-type"], "application/x-ndjson");
        // Where it stands, then one line for each channel that finished.
        assert_eq!(progress(&lines), [(0, 3), (1, 3), (2, 3), (3, 3)]);
        // Last, the feed, in the shape the same request without `stream` answers with.
        let last = lines.last().unwrap();
        assert_eq!(
            lines.iter().filter(|l| l.get("result").is_some()).count(),
            1
        );
        assert_eq!(runs(&bin.0), 3);
        let result = &last["result"];
        let mut keys: Vec<_> = result.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["channels", "entries", "errors", "fallbacks"]);
        assert_eq!(result["entries"].as_array().unwrap().len(), 3);
        let (status, body) = http(&base, "GET", &path(&base, "api/feed?refresh=1"), "");
        assert_eq!(status, 200);
        // The same feed. (What a refresh found out on the way, the channels
        // that needed the fallback, is only in the answer to the refresh.)
        let plain: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(plain["entries"], result["entries"]);
        assert_eq!(plain["channels"], result["channels"]);
        assert_eq!(
            runs(&bin.0),
            3,
            "fresh by now, so nothing was fetched again"
        );
    }

    #[test]
    fn a_stream_without_a_refresh_is_just_the_feed() {
        let (_s, base, _d, bin) = feed_server("", 2);
        let (headers, lines) = lines(&base, "stream=1");
        assert_eq!(headers["content-type"], "application/x-ndjson");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["result"]["channels"], 2);
        assert_eq!(crate::store::testing::listing_runs(&bin.0), 0);
    }

    /// A request that asks `query`, on a thread of its own: `(status, body)`.
    fn ask(base: &str, query: &str) -> std::thread::JoinHandle<(u16, String)> {
        let (base, query) = (base.to_string(), query.to_string());
        std::thread::spawn(move || {
            let (status, _, body) = fetch(&base, "GET", &format!("api/feed?{query}"), &[]);
            (status, String::from_utf8(body).unwrap())
        })
    }

    fn entries(body: &str) -> usize {
        let v: Value = serde_json::from_str(body).unwrap();
        v["entries"].as_array().unwrap().len()
    }

    #[test]
    fn a_refresh_while_one_is_running_joins_it_and_never_starts_a_second_burst() {
        use crate::store::testing::listing_runs as runs;
        let (_s, base, _d, bin) = feed_server("sleep 0.4", 4);
        let first = ask(&base, "refresh=1&stream=1");
        wait_for("the refresh to start", Duration::from_secs(10), || {
            runs(&bin.0) >= 1
        });
        // While it runs: a stream and a plain request. Both want what it is
        // already fetching.
        let (second, plain) = (ask(&base, "refresh=1&stream=1"), ask(&base, "refresh=1"));
        // A channel subscribed to meanwhile is not on the list that the job
        // went by, so the request for it can't join: it is held until the
        // job is over, and then fetches that one channel.
        let csv = format!("UC{:022},http://x,Chan5\n", 5);
        assert_eq!(
            http(&base, "POST", &path(&base, "api/subs/import"), &csv).0,
            200
        );
        let new = ask(&base, &format!("refresh=1&channel=UC{:022}", 5));
        let (status, first) = first.join().unwrap();
        assert_eq!(status, 200);
        let (_, second) = second.join().unwrap();
        let (plain_status, plain) = plain.join().unwrap();
        assert_eq!(plain_status, 200);
        let (new_status, new) = new.join().unwrap();
        assert_eq!(new_status, 200, "{new}");
        assert_eq!(
            runs(&bin.0),
            5,
            "one lookup per channel for all four together, and one for the new one"
        );
        assert_eq!(entries(&new), 5, "the new channel was fetched");

        let parse = |text: &str| -> Vec<Value> {
            text.lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        };
        let (first, second) = (parse(&first), parse(&second));
        // The one that joined sees the end of the same job, and the same feed.
        assert_eq!(progress(&second).last(), Some(&(4, 4)));
        assert_eq!(first.last(), second.last());
        let v: Value = serde_json::from_str(&plain).unwrap();
        assert_eq!(&v, &first.last().unwrap()["result"]);
    }

    #[test]
    fn a_refresh_of_every_channel_does_not_join_a_job_that_is_fetching_one() {
        use crate::store::testing::listing_runs as runs;
        let (_s, base, _d, bin) = feed_server("sleep 0.4", 3);
        let one = ask(&base, &format!("refresh=1&channel=UC{:022}", 1));
        wait_for("the lookup to start", Duration::from_secs(10), || {
            runs(&bin.0) >= 1
        });
        let all = ask(&base, "refresh=1");
        let (status, one) = one.join().unwrap();
        assert_eq!(status, 200, "{one}");
        let (status, all) = all.join().unwrap();
        assert_eq!(status, 200, "{all}");
        // The one channel, then the two that were still stale.
        assert_eq!(runs(&bin.0), 3);
        assert_eq!(entries(&all), 3);
    }

    #[test]
    fn a_forced_refresh_waits_for_a_stale_one_and_then_fetches_everything() {
        use crate::store::testing::listing_runs as runs;
        let (_s, base, _d, bin) = feed_server("sleep 0.3", 3);
        let stale = ask(&base, "refresh=1");
        wait_for("the refresh to start", Duration::from_secs(10), || {
            runs(&bin.0) >= 1
        });
        // The stale one would leave out what is fresh by then, and a forced
        // request is for all of it.
        let forced = ask(&base, "refresh=1&force=1");
        assert_eq!(stale.join().unwrap().0, 200);
        assert_eq!(forced.join().unwrap().0, 200);
        assert_eq!(
            runs(&bin.0),
            6,
            "three for the stale one, three for the forced one"
        );
    }

    #[test]
    fn a_forced_refresh_is_joined_by_the_requests_it_covers() {
        use crate::store::testing::listing_runs as runs;
        let (_s, base, _d, bin) = feed_server("sleep 0.3", 3);
        let forced = ask(&base, "refresh=1&force=1");
        wait_for("the refresh to start", Duration::from_secs(10), || {
            runs(&bin.0) >= 1
        });
        let stale = ask(&base, "refresh=1");
        let forced_again = ask(&base, "refresh=1&force=1");
        for r in [forced, stale, forced_again] {
            assert_eq!(r.join().unwrap().0, 200);
        }
        assert_eq!(
            runs(&bin.0),
            3,
            "all three asked for what the one job fetched"
        );
    }

    #[test]
    fn what_each_request_gets_is_shaped_by_what_it_asked_for() {
        let (_s, base, _d, bin) = feed_server("sleep 0.3", 3);
        let one = {
            let base = base.clone();
            std::thread::spawn(move || lines(&base, "refresh=1&stream=1&n=1"))
        };
        wait_for("the refresh to start", Duration::from_secs(10), || {
            crate::store::testing::listing_runs(&bin.0) >= 1
        });
        let (_, all) = lines(&base, "refresh=1&stream=1");
        let (_, one) = one.join().unwrap();
        let entries = |lines: &[Value]| {
            lines.last().unwrap()["result"]["entries"]
                .as_array()
                .unwrap()
                .len()
        };
        assert_eq!((entries(&one), entries(&all)), (1, 3));
    }

    #[test]
    fn the_job_goes_on_and_saves_when_every_client_has_hung_up() {
        use crate::store::testing::listing_runs as runs;
        let (s, base, _d, bin) = feed_server("sleep 0.3", 3);
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        write!(
            c,
            "GET {} HTTP/1.1\r\nHost: x\r\n\r\n",
            path(&base, "api/feed?refresh=1&stream=1")
        )
        .unwrap();
        let mut first = [0u8; 16];
        c.read_exact(&mut first).unwrap();
        drop(c);
        // Nobody is listening, and it finishes all the same.
        wait_for("the refresh to be done", Duration::from_secs(10), || {
            runs(&bin.0) == 3
                && std::fs::read_to_string(s.paths.feed_cache())
                    .is_ok_and(|c| c.matches("UC0000000000000000000").count() >= 3)
        });
        let cache: crate::feed::FeedCache = crate::store::load(&s.paths.feed_cache()).unwrap();
        assert_eq!(cache.fetched_at.len(), 3);
        // And the next request finds it all fresh instead of repeating it.
        assert_eq!(
            http(&base, "GET", &path(&base, "api/feed?refresh=1"), "").0,
            200
        );
        assert_eq!(runs(&bin.0), 3);
    }

    #[test]
    fn a_refresh_that_cannot_start_says_so_with_a_code_in_either_form() {
        let (_s, base, _d, bin) = feed_server("", 1);
        let unknown = format!("refresh=1&channel=UC{:022}", 9);
        // As a stream, it is the last line.
        let (_, lines) = lines(&base, &format!("{unknown}&stream=1"));
        let last = lines.last().unwrap();
        assert!(
            last["error"]
                .as_str()
                .unwrap()
                .contains("not one of your subscriptions"),
            "{last}"
        );
        assert_eq!(last["code"], "other");
        assert!(last.get("result").is_none());
        // Otherwise, as an error reply.
        let (status, body) = http(
            &base,
            "GET",
            &path(&base, &format!("api/feed?{unknown}")),
            "",
        );
        assert_eq!(status, 500);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["code"], "other");
        assert_eq!(crate::store::testing::listing_runs(&bin.0), 0);
        // And the failed job doesn't stay in the way.
        assert_eq!(
            http(&base, "GET", &path(&base, "api/feed?refresh=1"), "").0,
            200
        );
    }

    #[test]
    fn a_request_joins_a_job_only_on_the_route_it_runs_on() {
        use crate::net::TOR_PROXY;
        let jobs = Jobs::default();
        let direct = Net::new(None, true).unwrap();
        let tor = Net::new(Some(TOR_PROXY.into()), true).unwrap();
        let started = std::sync::atomic::AtomicUsize::new(0);
        let join = |net: &Net| {
            let claim = jobs.join_or_start(net, &Refresh::Stale, |_| {
                started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
            let Claim::Job(job) = claim else {
                panic!("a job that covers the request was in the way")
            };
            job
        };
        let first = join(&direct);
        assert!(Arc::ptr_eq(&first, &join(&direct)));
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 1);
        // The route changed: that job is being cut off, and this one is a new one.
        let on_tor = join(&tor);
        assert!(!Arc::ptr_eq(&first, &on_tor));
        assert!(Arc::ptr_eq(&on_tor, &join(&tor)));
        // One that is over is not joined.
        on_tor.finish(Ok(Refreshed {
            fallbacks: 0,
            errors: vec![],
            fresh: 0,
        }));
        assert!(!Arc::ptr_eq(&on_tor, &join(&tor)));
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn a_request_that_the_running_job_does_not_cover_is_told_to_wait_and_starts_nothing() {
        let jobs = Jobs::default();
        let net = Net::new(None, true).unwrap();
        let channel = |n: u32| Refresh::Channel(format!("UC{n:022}"));
        let started = std::sync::atomic::AtomicUsize::new(0);
        let ask = |refresh: &Refresh| {
            jobs.join_or_start(&net, refresh, |_| {
                started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        };
        let Claim::Job(one) = ask(&channel(1)) else {
            panic!("nothing was running")
        };
        // The same channel joins. Another channel, and a refresh of all of
        // them, each wait for this one instead of joining or starting.
        assert!(matches!(ask(&channel(1)), Claim::Job(j) if Arc::ptr_eq(&j, &one)));
        for other in [channel(2), Refresh::Stale, Refresh::Force] {
            assert!(
                matches!(ask(&other), Claim::Busy(j) if Arc::ptr_eq(&j, &one)),
                "{other:?}"
            );
        }
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Once it is over, the next request has a job of its own, which
        // covers what the ones after it ask for.
        one.finish(Ok(Refreshed {
            fallbacks: 0,
            errors: vec![],
            fresh: 0,
        }));
        let Claim::Job(all) = ask(&Refresh::Force) else {
            panic!("nothing was running any more")
        };
        assert!(!Arc::ptr_eq(&all, &one));
        assert!(matches!(ask(&Refresh::Stale), Claim::Job(j) if Arc::ptr_eq(&j, &all)));
        assert!(matches!(ask(&channel(1)), Claim::Busy(_)));
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn waiting_for_a_job_ends_when_it_is_over_or_nobody_is_waiting() {
        let job = FeedJob::new(Net::new(None, true).unwrap(), Refresh::Stale);
        assert!(!job.wait_over(|| true), "hung up before it was over");
        let ender = {
            let job = Arc::clone(&job);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                job.finish(Err(anyhow!("over")));
            })
        };
        assert!(job.wait_over(|| false));
        ender.join().unwrap();
        // Over already: no need to look whether anybody is there.
        assert!(job.wait_over(|| true));
    }

    #[test]
    fn a_job_whose_thread_dies_ends_with_an_error() {
        let job = FeedJob::new(Net::new(None, true).unwrap(), Refresh::Stale);
        drop(EndWithThread(Arc::clone(&job)));
        let state = job.wait(0);
        assert!(state.finished.unwrap().is_err());
        // What was said first stands: a result isn't replaced by a late failure.
        let job = FeedJob::new(Net::new(None, true).unwrap(), Refresh::Stale);
        job.finish(Ok(Refreshed {
            fallbacks: 1,
            errors: vec![],
            fresh: 0,
        }));
        drop(EndWithThread(Arc::clone(&job)));
        assert_eq!(job.wait(0).finished.unwrap().unwrap().fallbacks, 1);
    }
}
