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
use std::collections::BTreeSet;
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long a client waits for news before it looks whether the other end has
/// hung up. A hung-up client is noticed within this, and a job that is slow
/// to report doesn't leave a thread waiting for a client that left.
const POLL: Duration = Duration::from_millis(250);

/// How long a request waits behind a refresh that doesn't cover it before
/// it gives up, and answers with what is cached.
const WAIT_MAX: Duration = Duration::from_secs(10 * 60);

/// The refresh that is running, if there is one, and the one that goes next.
pub(super) struct Jobs {
    running: Mutex<Option<Arc<FeedJob>>>,
    /// What the requests waiting behind the running job want, which starts
    /// once that is over, before anything that asks after them. Locked
    /// after `running`, and only with it.
    next_up: Mutex<Option<NextUp>>,
    /// How many `NextUp`s there have been, which names each.
    made: std::sync::atomic::AtomicU64,
    /// `WAIT_MAX`, in milliseconds; only tests change it.
    wait_max: std::sync::atomic::AtomicU64,
}

impl Default for Jobs {
    fn default() -> Self {
        Jobs {
            running: Mutex::default(),
            next_up: Mutex::default(),
            made: Default::default(),
            wait_max: (WAIT_MAX.as_millis() as u64).into(),
        }
    }
}

/// The refresh that goes next, and how many requests wait for it.
struct NextUp {
    id: u64,
    refresh: Refresh,
    waiters: usize,
}

/// The `NextUp` that a request waits for, if it is counted among its waiters.
#[derive(Default)]
pub(super) struct Place(Option<u64>);

impl NextUp {
    /// Takes `place` out of its waiters, if it is among them.
    fn without(&mut self, place: &mut Place) {
        if place.0.take() == Some(self.id) {
            self.waiters -= 1;
        }
    }
}

pub(super) struct FeedJob {
    /// The route it runs on. A request after a route change must not join a
    /// job that is being cut off by it, and gets one of its own.
    net: Net,
    /// What it fetches, which is what a request must be covered by to join.
    refresh: Refresh,
    /// The channels that were subscribed to when it started. Its list of
    /// what to fetch is made from them, so a channel that isn't among them
    /// (one that was added since) is not fetched by it.
    listed: BTreeSet<String>,
    state: Mutex<JobState>,
    changed: Condvar,
    /// How many requests joined it after it started, for the tests.
    #[cfg(test)]
    joined: std::sync::atomic::AtomicUsize,
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
    fn new(net: Net, refresh: Refresh, listed: BTreeSet<String>) -> Arc<Self> {
        Arc::new(FeedJob {
            net,
            refresh,
            listed,
            state: Mutex::default(),
            changed: Condvar::new(),
            #[cfg(test)]
            joined: Default::default(),
        })
    }

    #[cfg(test)]
    fn joined(&self) -> usize {
        self.joined.load(std::sync::atomic::Ordering::SeqCst)
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

    /// Whether it fetches all that `wanted` asks for, with `subscribed` the
    /// channels that are subscribed to now. A refresh of all channels (or of
    /// those that are due) leaves out one that was added after it started.
    fn covers(&self, wanted: &Refresh, subscribed: &BTreeSet<String>) -> bool {
        self.refresh.covers(wanted)
            && match wanted {
                Refresh::Force | Refresh::Stale => subscribed.is_subset(&self.listed),
                Refresh::Channel(_) | Refresh::Off => true,
            }
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
    /// Waits until the job is over, until `until`, or until `gone` says
    /// nobody is waiting any more. `got_further` hears how far the job is
    /// each time that changes, and stops the wait by saying false.
    fn wait_over(
        &self,
        until: Instant,
        gone: &dyn Fn() -> bool,
        got_further: &mut dyn FnMut(usize, usize) -> bool,
    ) -> Waited {
        let (mut seen, mut told) = (0, None);
        loop {
            let state = self.wait(seen);
            seen = state.version;
            if state.finished.is_some() {
                return Waited::Over;
            }
            if let Some((done, total)) = state.progress.filter(|p| Some(*p) != told) {
                told = Some((done, total));
                if !got_further(done, total) {
                    return Waited::Gone;
                }
            }
            if gone() {
                return Waited::Gone;
            }
            if Instant::now() >= until {
                return Waited::TooLong;
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
    /// The job that is running on `net`'s route and covers `refresh` (with
    /// `subscribed` the channels subscribed to now), or a new one, which
    /// `start` is given to run. The check and the start happen
    /// under one lock, so that two requests arriving together share one job.
    ///
    /// A running job that doesn't cover `refresh` is not joined, and nor is a
    /// second one started beside it: that is `Claim::Busy`, and `refresh` is
    /// what goes next (see `wait_for_next`), with `place` saying whether the
    /// request is counted as waiting for it. (A job on another route is
    /// being cut off, and doesn't count.) With nothing running, the refresh
    /// that requests have waited for starts, whoever asks: one that comes
    /// later can't go ahead of them.
    fn join_or_start(
        &self,
        net: &Net,
        refresh: &Refresh,
        subscribed: &BTreeSet<String>,
        place: &mut Place,
        start: impl FnOnce(Arc<FeedJob>),
    ) -> Claim {
        let mut running = lock(&self.running);
        let mut next_up = lock(&self.next_up);
        // Asked again: it is no longer waiting, whatever comes of this.
        match next_up.as_mut() {
            Some(next) => next.without(place),
            None => place.0 = None,
        }
        if let Some(job) = running.as_ref()
            && job.running()
            && job.net.same_route(net)
        {
            return if job.covers(refresh, subscribed) {
                #[cfg(test)]
                job.joined.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if next_up.as_ref().is_some_and(|n| n.waiters == 0) {
                    *next_up = None;
                }
                Claim::Job(Arc::clone(job))
            } else {
                self.wait_for_next(&mut next_up, refresh, place);
                Claim::Busy(Arc::clone(job))
            };
        }
        let what = next_up
            .take()
            .map_or_else(|| refresh.clone(), |n| n.refresh);
        let job = FeedJob::new(net.clone(), what, subscribed.clone());
        *running = Some(Arc::clone(&job));
        start(Arc::clone(&job));
        if job.covers(refresh, subscribed) {
            Claim::Job(job)
        } else {
            self.wait_for_next(&mut next_up, refresh, place);
            Claim::Busy(job)
        }
    }

    /// A request that waited stops waiting (its client hung up, or it waited
    /// too long). A refresh that nobody waits for any more doesn't go next.
    fn leave(&self, place: &mut Place) {
        let _running = lock(&self.running);
        let mut next_up = lock(&self.next_up);
        match next_up.as_mut() {
            Some(next) => {
                next.without(place);
                if next.waiters == 0 {
                    *next_up = None;
                }
            }
            None => place.0 = None,
        }
    }

    fn wait_max(&self) -> Duration {
        Duration::from_millis(self.wait_max.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Makes `refresh` what goes next, or part of it: the first request to
    /// wait says what that is, and those after it wait for it if it covers
    /// them. A forced one makes a refresh of what is due a forced one, which
    /// still covers that. Anything else waits a turn more, unplaced.
    fn wait_for_next(&self, next_up: &mut Option<NextUp>, refresh: &Refresh, place: &mut Place) {
        let next = next_up.get_or_insert_with(|| NextUp {
            id: self.made.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            refresh: refresh.clone(),
            waiters: 0,
        });
        if *refresh == Refresh::Force && next.refresh == Refresh::Stale {
            next.refresh = Refresh::Force;
        }
        if next.refresh.covers(refresh) {
            next.waiters += 1;
            place.0 = Some(next.id);
        }
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
    /// which waits its turn if a refresh of something else is running
    /// (ahead of any request that comes after it, and for `WAIT_MAX` at
    /// most). With `&stream=1` the reply is a stream of lines:
    /// `{"waiting":{"done":N,"total":M}}` as the refresh it waits for goes
    /// on, `{"progress":{"done":N,"total":M}}` as its own channels finish,
    /// and last `{"result": …}` (the feed, as it is without `stream`) or
    /// `{"error": …, "code": …}`.
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
            return reply(out, stream, false, feed.map(|f| json!(f)));
        }
        let watch = Watch::new(out);
        // Whether the stream's head is out: a request that waits sends it
        // at once, and says how the refresh it waits for is going.
        let mut head = false;
        let mut place = Place::default();
        let until = Instant::now() + self.jobs.wait_max();
        let job = loop {
            // Taken again after a wait, for the route may have changed and
            // channels may have been added.
            let rt = self.runtime();
            let subscribed = match app::load_subs(&self.paths) {
                Ok(subs) => subs.channels.into_iter().map(|s| s.id).collect(),
                Err(e) => {
                    self.jobs.leave(&mut place);
                    return reply(out, stream, head, Err(e));
                }
            };
            let claim =
                self.jobs
                    .join_or_start(&rt.net, &refresh, &subscribed, &mut place, |job| {
                        self.start_job(&rt, job)
                    });
            match claim {
                Claim::Job(job) => break job,
                // A job that doesn't fetch what this wants: wait for it to be
                // over, unless the client hangs up first.
                Claim::Busy(busy) => {
                    match wait_behind(out, &busy, stream, &mut head, &watch, until) {
                        Waited::Over => {}
                        Waited::Gone => {
                            self.jobs.leave(&mut place);
                            return Ok(());
                        }
                        Waited::TooLong => {
                            self.jobs.leave(&mut place);
                            let left = Refreshed {
                                fallbacks: 0,
                                errors: vec![TOO_LONG.into()],
                                fresh: 0,
                            };
                            let feed =
                                app::feed_result(&self.paths, shape.no_shorts, shape.limit, &left);
                            return reply(out, stream, head, feed.map(|f| json!(f)));
                        }
                    }
                }
            }
        };
        if stream {
            self.stream_job(out, &job, shape, head)
        } else {
            self.wait_for_job(out, &job, shape)
        }
    }

    fn start_job(self: &Arc<Self>, rt: &Arc<Runtime>, job: Arc<FeedJob>) {
        let (server, rt) = (Arc::clone(self), Arc::clone(rt));
        let refresh = job.refresh.clone();
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
                return reply(out, false, false, self.job_result(finished, shape));
            }
            if watch.gone() {
                return Ok(());
            }
        }
    }

    /// Streams the job's progress, then its result. `head` says whether the
    /// stream's head is out already.
    fn stream_job(
        &self,
        out: &mut TcpStream,
        job: &FeedJob,
        shape: Shape,
        head: bool,
    ) -> Result<()> {
        if !head && out.write_all(NDJSON_HEAD.as_bytes()).is_err() {
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

/// What a request says when it has waited `WAIT_MAX` behind another refresh.
const TOO_LONG: &str = "Another refresh was still running after 10 minutes, so this one wasn't made; this is what was already fetched.";

/// How waiting behind another job ended.
enum Waited {
    Over,
    Gone,
    TooLong,
}

/// Waits for `busy` to be over, until `until`, or until the client hangs up.
/// With `stream`, the head goes out at once (`head` says it has), and a
/// `{"waiting":{"done":N,"total":M}}` line each time the other job gets
/// further, so that a client that waits a long time hears something.
fn wait_behind(
    out: &mut TcpStream,
    busy: &FeedJob,
    stream: bool,
    head: &mut bool,
    watch: &Watch,
    until: Instant,
) -> Waited {
    if stream && !*head {
        if out.write_all(NDJSON_HEAD.as_bytes()).is_err() {
            return Waited::Gone;
        }
        *head = true;
    }
    busy.wait_over(until, &|| watch.gone(), &mut |done, total| {
        let line = json!({ "waiting": { "done": done, "total": total } });
        !stream || write_line(out, &line).is_ok()
    })
}

/// Answers with a feed that is ready, or says why it isn't: as the whole
/// reply, or as the one line of a stream (whose head is out if `head`).
fn reply(out: &mut TcpStream, stream: bool, head: bool, feed: Result<Value>) -> Result<()> {
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
            if !head {
                out.write_all(NDJSON_HEAD.as_bytes())?;
            }
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
        let (s, base, _d, bin) = feed_server("sleep 0.4", 4);
        let first = ask(&base, "refresh=1&stream=1");
        wait_for("the refresh to start", Duration::from_secs(10), || {
            runs(&bin.0) >= 1
        });
        // While it runs: a stream and a plain request. Both want what it is
        // already fetching.
        let (second, plain) = (ask(&base, "refresh=1&stream=1"), ask(&base, "refresh=1"));
        // They must have joined before the subscription below, or they would
        // be asking for more than this job started with.
        wait_for("both to join it", Duration::from_secs(10), || {
            lock(&s.jobs.running)
                .as_ref()
                .is_some_and(|job| job.joined() == 2)
        });
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
    fn channels_imported_while_a_refresh_runs_are_fetched_by_the_one_that_follows() {
        use crate::store::testing::listing_runs as runs;
        let (_s, base, _d, bin) = feed_server("sleep 0.3", 3);
        let running = ask(&base, "refresh=1&stream=1");
        wait_for("the refresh to start", Duration::from_secs(10), || {
            runs(&bin.0) >= 1
        });
        // What the window asks for after an import: the imported channels
        // have never been fetched, so they are stale, but they are not on the
        // list that the running refresh went by.
        let csv = format!("UC{:022},http://x,Chan4\nUC{:022},http://x,Chan5\n", 4, 5);
        assert_eq!(
            http(&base, "POST", &path(&base, "api/subs/import"), &csv).0,
            200
        );
        let after = ask(&base, "refresh=1&stream=1");
        assert_eq!(running.join().unwrap().0, 200);
        let (status, after) = after.join().unwrap();
        assert_eq!(status, 200);
        assert_eq!(
            runs(&bin.0),
            5,
            "three for the running one, and the two that were imported"
        );
        let last: Value = serde_json::from_str(after.lines().last().unwrap()).unwrap();
        assert_eq!(last["result"]["entries"].as_array().unwrap().len(), 5);
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
            let claim = jobs.join_or_start(
                net,
                &Refresh::Stale,
                &BTreeSet::new(),
                &mut Place::default(),
                |_| {
                    started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
            );
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
            jobs.join_or_start(
                &net,
                refresh,
                &BTreeSet::new(),
                &mut Place::default(),
                |_| {
                    started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
            )
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
        // Once it is over, what the first of them asked for goes first,
        // whoever asks next.
        let done = || {
            Ok(Refreshed {
                fallbacks: 0,
                errors: vec![],
                fresh: 0,
            })
        };
        one.finish(done());
        let Claim::Busy(two) = ask(&Refresh::Force) else {
            panic!("the one that waited was passed over")
        };
        assert_eq!(two.refresh, channel(2));
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 2);
        // Then the next request has a job of its own, which covers what the
        // ones after it ask for.
        two.finish(done());
        let Claim::Job(all) = ask(&Refresh::Force) else {
            panic!("nothing was running any more")
        };
        assert!(!Arc::ptr_eq(&all, &one));
        assert!(matches!(ask(&Refresh::Stale), Claim::Job(j) if Arc::ptr_eq(&j, &all)));
        assert!(matches!(ask(&channel(1)), Claim::Busy(_)));
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    fn over() -> Result<Refreshed> {
        Ok(Refreshed {
            fallbacks: 0,
            errors: vec![],
            fresh: 0,
        })
    }

    #[test]
    fn a_request_that_comes_after_those_waiting_does_not_go_ahead_of_them() {
        let jobs = Jobs::default();
        let net = Net::new(None, true).unwrap();
        let ask = |refresh: &Refresh, place: &mut Place| {
            jobs.join_or_start(&net, refresh, &BTreeSet::new(), place, |_| {})
        };
        let Claim::Job(stale) = ask(&Refresh::Stale, &mut Place::default()) else {
            panic!("nothing was running")
        };
        let mut forced = Place::default();
        assert!(matches!(ask(&Refresh::Force, &mut forced), Claim::Busy(_)));
        stale.finish(over());
        // Another request for what is due, before the forced one has looked
        // again: it gets the forced refresh, which covers it.
        let Claim::Job(next) = ask(&Refresh::Stale, &mut Place::default()) else {
            panic!("a forced refresh covers it")
        };
        assert_eq!(next.refresh, Refresh::Force);
        assert!(
            matches!(ask(&Refresh::Force, &mut forced), Claim::Job(j) if Arc::ptr_eq(&j, &next))
        );
        assert!(lock(&jobs.next_up).is_none());
    }

    #[test]
    fn a_forced_request_that_waits_makes_the_next_refresh_a_forced_one() {
        let jobs = Jobs::default();
        let net = Net::new(None, true).unwrap();
        let ask = |refresh: &Refresh, place: &mut Place| {
            jobs.join_or_start(&net, refresh, &BTreeSet::new(), place, |_| {})
        };
        let one = Refresh::Channel(format!("UC{:022}", 1));
        let Claim::Job(running) = ask(&one, &mut Place::default()) else {
            panic!("nothing was running")
        };
        let (mut stale, mut forced) = (Place::default(), Place::default());
        assert!(matches!(ask(&Refresh::Stale, &mut stale), Claim::Busy(_)));
        assert!(matches!(ask(&Refresh::Force, &mut forced), Claim::Busy(_)));
        running.finish(over());
        let Claim::Job(next) = ask(&Refresh::Stale, &mut stale) else {
            panic!("the next one covers it")
        };
        assert_eq!(next.refresh, Refresh::Force);
        assert!(
            matches!(ask(&Refresh::Force, &mut forced), Claim::Job(j) if Arc::ptr_eq(&j, &next))
        );
    }

    #[test]
    fn a_refresh_that_nobody_waits_for_any_more_does_not_go_next() {
        let jobs = Jobs::default();
        let net = Net::new(None, true).unwrap();
        let ask = |refresh: &Refresh, place: &mut Place| {
            jobs.join_or_start(&net, refresh, &BTreeSet::new(), place, |_| {})
        };
        let Claim::Job(stale) = ask(&Refresh::Stale, &mut Place::default()) else {
            panic!("nothing was running")
        };
        let (mut first, mut second) = (Place::default(), Place::default());
        assert!(matches!(ask(&Refresh::Force, &mut first), Claim::Busy(_)));
        assert!(matches!(ask(&Refresh::Force, &mut second), Claim::Busy(_)));
        // One hangs up: the other still waits for it.
        jobs.leave(&mut first);
        assert!(lock(&jobs.next_up).is_some());
        jobs.leave(&mut second);
        assert!(lock(&jobs.next_up).is_none());
        stale.finish(over());
        let Claim::Job(next) = ask(&Refresh::Stale, &mut Place::default()) else {
            panic!("nothing was running")
        };
        assert_eq!(next.refresh, Refresh::Stale);
    }

    #[test]
    fn a_forced_refresh_that_waits_hears_how_the_other_one_goes_and_then_runs() {
        use crate::store::testing::listing_runs as runs;
        let (s, base, _d, bin) = feed_server("sleep 0.3", 3);
        let stale = ask(&base, "refresh=1");
        wait_for("the refresh to start", Duration::from_secs(10), || {
            runs(&bin.0) >= 1
        });
        let forced = {
            let base = base.clone();
            std::thread::spawn(move || lines(&base, "refresh=1&force=1&stream=1").1)
        };
        wait_for("it to wait", Duration::from_secs(10), || {
            lock(&s.jobs.next_up).is_some()
        });
        // One for what is due, which comes after it: it goes no further ahead
        // than the running one, and joins it, or the forced one after.
        let later = ask(&base, "refresh=1");
        assert_eq!(stale.join().unwrap().0, 200);
        let forced = forced.join().unwrap();
        assert_eq!(later.join().unwrap().0, 200);
        assert_eq!(
            runs(&bin.0),
            6,
            "three for each, and none for the later one"
        );
        let waiting: Vec<&Value> = forced
            .iter()
            .filter(|l| l.get("waiting").is_some())
            .collect();
        assert!(!waiting.is_empty(), "{forced:?}");
        assert_eq!(waiting[0]["waiting"]["total"], 3);
        // Then its own refresh, of every channel, and the feed.
        assert_eq!(progress(&forced).last(), Some(&(3, 3)));
        assert!(forced.last().unwrap().get("result").is_some(), "{forced:?}");
    }

    #[test]
    fn a_request_that_waits_too_long_gets_what_is_cached_and_why() {
        use crate::store::testing::listing_runs as runs;
        let (s, base, _d, bin) = feed_server("sleep 0.5", 3);
        s.jobs
            .wait_max
            .store(300, std::sync::atomic::Ordering::Relaxed);
        let stale = ask(&base, "refresh=1");
        wait_for("the refresh to start", Duration::from_secs(10), || {
            runs(&bin.0) >= 1
        });
        let (_, lines) = lines(&base, &format!("refresh=1&channel=UC{:022}&stream=1", 1));
        let last = lines.last().unwrap();
        assert_eq!(
            last["result"]["errors"],
            serde_json::json!([TOO_LONG]),
            "{lines:?}"
        );
        assert_eq!(last["result"]["channels"], 3);
        assert!(lock(&s.jobs.next_up).is_none(), "it doesn't go next");
        assert_eq!(stale.join().unwrap().0, 200);
        assert_eq!(runs(&bin.0), 3);
    }

    #[test]
    fn a_refresh_of_all_channels_is_joined_only_if_it_started_with_all_that_are_subscribed_to_now()
    {
        let net = Net::new(None, true).unwrap();
        let ids =
            |ids: &[&str]| -> BTreeSet<String> { ids.iter().map(|i| i.to_string()).collect() };
        for (running, other) in [
            (Refresh::Stale, Refresh::Force),
            (Refresh::Force, Refresh::Stale),
        ] {
            let jobs = Jobs::default();
            let ask = |refresh: &Refresh, subscribed: &[&str]| {
                jobs.join_or_start(
                    &net,
                    refresh,
                    &ids(subscribed),
                    &mut Place::default(),
                    |_| {},
                )
            };
            let Claim::Job(job) = ask(&running, &["a", "b"]) else {
                panic!("nothing was running")
            };
            let joined = |claim: Claim| matches!(claim, Claim::Job(j) if Arc::ptr_eq(&j, &job));
            let waits = |claim: Claim| matches!(claim, Claim::Busy(j) if Arc::ptr_eq(&j, &job));
            // The same channels, or fewer (one was unsubscribed): joins.
            assert!(joined(ask(&running, &["a", "b"])), "{running:?}");
            assert!(joined(ask(&running, &["a"])), "{running:?}");
            assert!(joined(ask(&running, &[])), "{running:?}");
            // One that came after it started: waits.
            assert!(waits(ask(&running, &["a", "b", "c"])), "{running:?}");
            assert!(waits(ask(&running, &["c"])), "{running:?}");
            // What the running one covers anyway follows the same rule.
            if running.covers(&other) {
                assert!(joined(ask(&other, &["a", "b"])));
                assert!(waits(ask(&other, &["a", "b", "c"])));
            }
            // A request for one channel doesn't go by the list.
            assert!(waits(ask(&Refresh::Channel("a".into()), &["a", "b", "c"])));
        }
        // A job for one channel is the same one for that channel, whoever
        // else has subscribed meanwhile.
        let jobs = Jobs::default();
        let one = Refresh::Channel("a".into());
        let Claim::Job(job) =
            jobs.join_or_start(&net, &one, &ids(&["a"]), &mut Place::default(), |_| {})
        else {
            panic!("nothing was running")
        };
        let again =
            jobs.join_or_start(&net, &one, &ids(&["a", "b"]), &mut Place::default(), |_| {});
        assert!(matches!(again, Claim::Job(j) if Arc::ptr_eq(&j, &job)));
    }

    #[test]
    fn waiting_for_a_job_ends_when_it_is_over_or_nobody_is_waiting() {
        let job = FeedJob::new(
            Net::new(None, true).unwrap(),
            Refresh::Stale,
            BTreeSet::new(),
        );
        let far = Instant::now() + Duration::from_secs(60);
        let wait = |gone: bool, said: &mut Vec<(usize, usize)>| {
            job.wait_over(far, &|| gone, &mut |done, total| {
                said.push((done, total));
                true
            })
        };
        let mut said = Vec::new();
        assert!(
            matches!(wait(true, &mut said), Waited::Gone),
            "hung up before it was over"
        );
        let ender = {
            let job = Arc::clone(&job);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                job.progress(1, 2);
                std::thread::sleep(Duration::from_millis(100));
                job.finish(Err(anyhow!("over")));
            })
        };
        assert!(matches!(wait(false, &mut said), Waited::Over));
        ender.join().unwrap();
        assert_eq!(said, [(1, 2)], "it was told how far the job got");
        // Over already: no need to look whether anybody is there.
        assert!(matches!(wait(true, &mut said), Waited::Over));
        // A wait that is not over in time ends too.
        let other = FeedJob::new(
            Net::new(None, true).unwrap(),
            Refresh::Stale,
            BTreeSet::new(),
        );
        let now = Instant::now();
        assert!(matches!(
            other.wait_over(now, &|| false, &mut |_, _| true),
            Waited::TooLong
        ));
    }

    #[test]
    fn a_job_whose_thread_dies_ends_with_an_error() {
        let job = FeedJob::new(
            Net::new(None, true).unwrap(),
            Refresh::Stale,
            BTreeSet::new(),
        );
        drop(EndWithThread(Arc::clone(&job)));
        let state = job.wait(0);
        assert!(state.finished.unwrap().is_err());
        // What was said first stands: a result isn't replaced by a late failure.
        let job = FeedJob::new(
            Net::new(None, true).unwrap(),
            Refresh::Stale,
            BTreeSet::new(),
        );
        job.finish(Ok(Refreshed {
            fallbacks: 1,
            errors: vec![],
            fresh: 0,
        }));
        drop(EndWithThread(Arc::clone(&job)));
        assert_eq!(job.wait(0).finished.unwrap().unwrap().fallbacks, 1);
    }
}
