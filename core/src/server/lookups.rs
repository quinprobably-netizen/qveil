//! The lookups of the videos played lately, so that playing one again (the
//! next video and back, a reload of the page, a click on the same card)
//! needs no yt-dlp run, which over Tor takes a few seconds.
//!
//! A lookup is only good for as long as its stream URLs: googlevideo signs
//! them with an `expire` time, and only for the address that looked them up.
//! Over a SOCKS proxy that address is the circuit, and Tor stops putting new
//! connections on a circuit ten minutes after its first use, so there a
//! lookup is kept for less than that. It is kept with the relay client that
//! made it, which is what holds that circuit's credentials, and only ever
//! used on the route it was made on. It lives in memory only.

use super::relay::Live;
use crate::net::Net;
use crate::util::lock;
use crate::ytdlp::{CANCELLED, Streams};
use anyhow::{Result, anyhow};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How many lookups are kept.
const KEPT: usize = 8;
/// How long before its URLs expire a lookup is no longer used: a video
/// started on URLs that expire while it plays has to be looked up again then.
const MARGIN: Duration = Duration::from_secs(10 * 60);
/// How long a lookup made over a SOCKS proxy is kept: under Tor's ten
/// minutes (`MaxCircuitDirtiness`), with time to spare for the connection
/// the player opens.
const CIRCUIT: Duration = Duration::from_secs(9 * 60);

struct Entry {
    id: String,
    /// The yt-dlp format selector it was made with: a different quality cap
    /// is a different lookup.
    format: String,
    route: Net,
    until: Instant,
    streams: Streams,
    live: Live,
}

#[derive(Default)]
pub(super) struct Lookups {
    entries: Mutex<VecDeque<Entry>>,
    /// The lookups ahead that are running (see `Running`).
    running: Mutex<Vec<Arc<Running>>>,
}

/// How often a play waiting on a lookup ahead asks whether it was given up.
const JOIN_POLL: Duration = Duration::from_millis(50);

/// A lookup ahead (`api/prepare`) that is still running. A play of the same
/// video on the same route and quality waits for it instead of running one
/// of its own, which would start from zero: N pressed while the next video
/// is being looked up is the usual way to get here. The lookup ends early
/// only once nobody wants it: neither whoever started it nor a play
/// waiting on it.
pub(super) struct Running {
    id: String,
    format: String,
    route: Net,
    state: Mutex<RunState>,
    changed: Condvar,
}

#[derive(Default)]
struct RunState {
    /// The plays waiting on it.
    waiting: usize,
    /// Set once it was given up, with nobody waiting: a play that comes
    /// after that runs its own lookup.
    abandoned: bool,
    /// How it ended, once it has. The error as text: it is handed to every
    /// play that waited.
    outcome: Option<std::result::Result<(Streams, Live), String>>,
}

impl Running {
    /// Whether the lookup is to stop, now that whoever started it does or
    /// doesn't want it (`owner_gone`): only when no play is waiting on it.
    pub(super) fn give_up(&self, owner_gone: bool) -> bool {
        if !owner_gone {
            return false;
        }
        let mut st = lock(&self.state);
        if st.waiting > 0 {
            return false;
        }
        st.abandoned = true;
        true
    }

    fn finish(&self, outcome: std::result::Result<(Streams, Live), String>) {
        let mut st = lock(&self.state);
        if st.outcome.is_none() {
            st.outcome = Some(outcome);
        }
        self.changed.notify_all();
    }

    /// Waits for the lookup's result. `None` if it was given up before this
    /// could wait on it. `abort` ends the wait (as it would a lookup of its
    /// own), and with nobody else waiting, the lookup too.
    fn join(&self, abort: &dyn Fn() -> bool) -> Option<Result<(Streams, Live)>> {
        let mut st = lock(&self.state);
        if st.abandoned && st.outcome.is_none() {
            return None;
        }
        st.waiting += 1;
        loop {
            if let Some(outcome) = &st.outcome {
                let got = outcome.clone().map_err(|e| anyhow!(e));
                st.waiting -= 1;
                return Some(got);
            }
            if abort() {
                st.waiting -= 1;
                return Some(Err(anyhow!(CANCELLED)));
            }
            st = match self.changed.wait_timeout(st, JOIN_POLL) {
                Ok((st, _)) => st,
                Err(e) => e.into_inner().0,
            };
        }
    }
}

/// Held by a lookup ahead while it runs: when it is dropped, plays waiting
/// on the lookup get its result (or, if none was set, that it was
/// cancelled), and it is no longer found by new ones.
pub(super) struct RunGuard<'a> {
    lookups: &'a Lookups,
    pub(super) run: Arc<Running>,
}

impl RunGuard<'_> {
    /// Hands `outcome` to the plays waiting on it.
    pub(super) fn finish(&self, outcome: &Result<(Streams, Live)>) {
        self.run.finish(match outcome {
            Ok(found) => Ok(found.clone()),
            Err(e) => Err(format!("{e:#}")),
        });
    }
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.run.finish(Err(CANCELLED.into()));
        lock(&self.lookups.running).retain(|r| !Arc::ptr_eq(r, &self.run));
    }
}

impl Lookups {
    /// Marks a lookup ahead of `id` as running, until the guard is dropped.
    pub(super) fn begin(&self, id: &str, format: &str, route: &Net) -> RunGuard<'_> {
        let run = Arc::new(Running {
            id: id.to_string(),
            format: format.to_string(),
            route: route.clone(),
            state: Mutex::default(),
            changed: Condvar::new(),
        });
        lock(&self.running).push(Arc::clone(&run));
        RunGuard { lookups: self, run }
    }

    /// The result of the lookup ahead of `id` with `format` on `route`, if
    /// one is running: waited for, until `abort` says to stop waiting.
    /// `None` when none is running (or it was given up).
    pub(super) fn join(
        &self,
        id: &str,
        format: &str,
        route: &Net,
        abort: &dyn Fn() -> bool,
    ) -> Option<Result<(Streams, Live)>> {
        let run = lock(&self.running)
            .iter()
            .rev()
            .find(|r| r.id == id && r.format == format && r.route.same_route(route))
            .cloned()?;
        run.join(abort)
    }

    /// The lookup of `id` with `format` on `route`, if one is still good.
    pub(super) fn get(&self, id: &str, format: &str, route: &Net) -> Option<(Streams, Live)> {
        let mut entries = lock(&self.entries);
        let now = Instant::now();
        entries.retain(|e| e.until > now);
        entries
            .iter()
            .find(|e| e.id == id && e.format == format && e.route.same_route(route))
            .map(|e| (e.streams.clone(), e.live.clone()))
    }

    /// Keeps a lookup that was just made, for as long as it is good.
    pub(super) fn put(&self, id: &str, format: &str, route: &Net, streams: &Streams, live: &Live) {
        let Some(good_for) = good_for(route, streams, unix_now()) else {
            return;
        };
        let mut entries = lock(&self.entries);
        entries.retain(|e| !(e.id == id && e.format == format));
        entries.push_back(Entry {
            id: id.to_string(),
            format: format.to_string(),
            route: route.clone(),
            until: Instant::now() + good_for,
            streams: streams.clone(),
            live: live.clone(),
        });
        while entries.len() > KEPT {
            entries.pop_front();
        }
    }

    /// Forgets the lookups of `id`: its URLs stopped working.
    pub(super) fn forget(&self, id: &str) {
        lock(&self.entries).retain(|e| e.id != id);
    }

    pub(super) fn clear(&self) {
        lock(&self.entries).clear();
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How long a lookup made at `now` (Unix seconds) may be used. `None` when
/// that can't be told: a URL without an `expire` is not kept at all.
fn good_for(route: &Net, streams: &Streams, now: u64) -> Option<Duration> {
    let mut urls = vec![streams.video.as_str()];
    urls.extend(streams.audio.as_deref());
    let expire = urls.into_iter().map(expires).min()??;
    let left = Duration::from_secs(expire.saturating_sub(now)).checked_sub(MARGIN)?;
    let left = if route.is_socks() {
        left.min(CIRCUIT)
    } else {
        left
    };
    (!left.is_zero()).then_some(left)
}

/// The `expire` parameter of a googlevideo URL, in Unix seconds.
fn expires(url: &str) -> Option<u64> {
    url::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == "expire")?
        .1
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::TOR_PROXY;

    fn streams(video: &str, audio: Option<&str>) -> Streams {
        Streams {
            title: "T".into(),
            channel: None,
            channel_id: None,
            duration: None,
            video: video.into(),
            audio: audio.map(String::from),
            headers: vec![],
            description: None,
            views: None,
            likes: None,
            published: None,
            chapters: vec![],
            chapters_later: false,
        }
    }

    #[test]
    fn a_lookup_is_good_until_its_urls_expire_less_a_margin() {
        let direct = Net::new(None, true).unwrap();
        let now = 1_000_000;
        let url = |expire: u64| {
            format!("https://rr1.googlevideo.test/videoplayback?expire={expire}&sig=x")
        };
        let six_hours = url(now + 6 * 3600);
        assert_eq!(
            good_for(&direct, &streams(&six_hours, None), now),
            Some(Duration::from_secs(6 * 3600) - MARGIN)
        );
        // The stream that expires first decides.
        let one_hour = url(now + 3600);
        assert_eq!(
            good_for(&direct, &streams(&six_hours, Some(&one_hour)), now),
            Some(Duration::from_secs(3600) - MARGIN)
        );
        // Too close to expiring, expired, or no saying when: not kept.
        for v in [
            url(now + 300),
            url(now - 1),
            "https://gv.test/v?sig=x".into(),
        ] {
            assert_eq!(good_for(&direct, &streams(&v, None), now), None, "{v}");
        }
        assert_eq!(
            good_for(
                &direct,
                &streams(&six_hours, Some("https://gv.test/a")),
                now
            ),
            None
        );
    }

    #[test]
    fn over_socks_a_lookup_is_kept_for_less_than_the_life_of_its_circuit() {
        let now = 1_000_000;
        let url = format!("https://gv.test/v?expire={}", now + 6 * 3600);
        for proxy in [TOR_PROXY, "socks5h://me:pw@10.0.0.2:1080"] {
            let net = Net::new(Some(proxy.into()), true).unwrap();
            assert_eq!(
                good_for(&net, &streams(&url, None), now),
                Some(CIRCUIT),
                "{proxy}"
            );
        }
        assert!(CIRCUIT < Duration::from_secs(10 * 60));
        let http = Net::new(Some("http://10.0.0.1:3128".into()), true).unwrap();
        assert!(good_for(&http, &streams(&url, None), now).unwrap() > CIRCUIT);
    }
}

#[cfg(test)]
mod played_again {
    use crate::config::Config;
    use crate::server::testkit::*;
    use crate::store::testing::TempDir;
    use crate::util::lock;

    fn later() -> u64 {
        super::unix_now() + 6 * 3600
    }

    #[test]
    fn a_video_played_again_is_not_looked_up_again_on_the_same_route_and_quality() {
        let bin = TempDir::new();
        let ytdlp = fake_ytdlp(&bin.0, &format!("https://gv.test/v?expire={}&n=", later()));
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let post = |body: &str| http(&base, "POST", &path(&base, "api/config"), body).0;
        let first = s.open(&test_video()).unwrap();
        let again = s.open(&test_video()).unwrap();
        assert_eq!(runs(&bin.0), 1);
        // A session of its own, on the same streams.
        assert_ne!(first.video, again.video);
        assert_eq!(again.title, "T");
        let sid = rel(&base, &again.video)
            .split('/')
            .nth(1)
            .unwrap()
            .to_string();
        assert!(lock(&s.session(&sid).unwrap().live).video.ends_with("&n=1"));

        // Another quality cap is another lookup, which is then kept too.
        assert_eq!(post(r#"{"maxHeight":720}"#), 200);
        s.open(&test_video()).unwrap();
        s.open(&test_video()).unwrap();
        assert_eq!(runs(&bin.0), 2);

        // A lookup is never used on another route, nor on the old one again
        // after a switch.
        assert_eq!(post(r#"{"route":"tor"}"#), 200);
        s.open(&test_video()).unwrap();
        assert_eq!(runs(&bin.0), 3);
        assert_eq!(post(r#"{"route":"direct"}"#), 200);
        s.open(&test_video()).unwrap();
        assert_eq!(runs(&bin.0), 4);

        // Clearing the history forgets them as well.
        assert_eq!(
            http(&base, "POST", &path(&base, "api/history/clear"), "").0,
            200
        );
        s.open(&test_video()).unwrap();
        assert_eq!(runs(&bin.0), 5);
    }

    #[test]
    fn a_lookup_whose_urls_stopped_working_is_not_used_again() {
        let up = Upstream::start();
        let bin = TempDir::new();
        // `gen=1` gives a 403 past the first byte, as a retired circuit would.
        let url = up.url(&format!("/big?expire={}&gen=", later()));
        let ytdlp = fake_ytdlp(&bin.0, &url);
        let (s, base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let info = s.open(&test_video()).unwrap();
        let (status, _, body) = fetch(
            &base,
            "GET",
            rel(&base, &info.video),
            &[("Range", "bytes=5-9")],
        );
        assert_eq!((status, body), (206, big_bytes(5, 10)));
        assert_eq!(runs(&bin.0), 2, "resolved again after the 403");
        s.open(&test_video()).unwrap();
        assert_eq!(runs(&bin.0), 3);
    }
}
