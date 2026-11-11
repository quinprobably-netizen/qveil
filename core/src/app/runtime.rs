//! What a command runs on: the settings, the route and yt-dlp on it, and
//! how a refresh spaces out its searches.

use crate::config::Config;
use crate::net::{self, Net};
use crate::store::Paths;
use crate::ytdlp::Ytdlp;
use anyhow::Result;
use reqwest::blocking::Client;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// How long what a refresh has fetched may wait before it is saved. Saving
/// rewrites the whole cache, which for a few hundred channels is a lot to do
/// after each one; a burst of channels shares one write, and an engine that is
/// killed mid-refresh loses at most this much of it.
const SAVE_EVERY: Duration = Duration::from_secs(2);

/// How long a lens or topic refresh waits before each of its searches after
/// the first, at random between these: searches that go out back to back,
/// and in the same order each time, are easier to tell from a person's.
pub const SEARCH_PAUSE: (Duration, Duration) = (Duration::from_secs(2), Duration::from_secs(8));

/// What a refresh does before each of its searches after the first: waits
/// (see `SEARCH_PAUSE`), and ends early once what it is given says the
/// answer is no longer wanted. It must not hold a lock while it waits.
pub type Pause = Arc<dyn Fn(&dyn Fn() -> bool) + Send + Sync>;

pub struct Runtime {
    pub cfg: Config,
    pub net: Net,
    pub yt: Ytdlp,
    /// The client short requests share where the route allows it (see
    /// `web_client`). Seen from the rest of `app` for its tests, which make
    /// runtimes out of others.
    pub(super) shared: OnceLock<Client>,
    /// Where the engine's own requests go: the channel feeds and the
    /// thumbnails. Only tests change them, to stand a local server in for YouTube.
    pub feed_base: String,
    pub thumb_base: String,
    /// How often a refresh saves (`SAVE_EVERY`); only tests change it.
    pub save_every: Duration,
    /// The wait between a refresh's searches (`SEARCH_PAUSE`); only tests
    /// change it.
    pub pause: Pause,
}

impl Runtime {
    /// `proxy` / `tor` are command-line overrides of the config's route.
    pub fn new(cfg: Config, paths: &Paths, proxy: Option<String>, tor: bool) -> Result<Self> {
        let (proxy, isolate) = if tor {
            (Some(net::TOR_PROXY.to_string()), true)
        } else {
            (proxy.or_else(|| cfg.proxy.clone()), cfg.isolate_circuits)
        };
        let net = Net::new(proxy, isolate)?;
        let yt = Ytdlp {
            bin: cfg.ytdlp.clone(),
            cache_dir: paths.ytdlp_cache(),
            net: net.clone(),
            calls: Arc::default(),
            quick_lookup: cfg.quick_lookup,
        };
        Ok(Self {
            cfg,
            net,
            yt,
            shared: OnceLock::new(),
            feed_base: "https://www.youtube.com".into(),
            thumb_base: "https://i.ytimg.com".into(),
            save_every: SAVE_EVERY,
            pause: search_pause(),
        })
    }

    /// The client for one short request: a feed or a thumbnail. Where the
    /// route isolates requests (Tor), each gets a client of its own, and with
    /// it a circuit of its own. Otherwise they share one and reuse its
    /// connections: they leave from the same address either way, and a new
    /// client's TLS handshake was most of what a thumbnail cost.
    pub fn web_client(&self) -> Result<Client> {
        if self.net.isolates() {
            return self.net.client();
        }
        if let Some(c) = self.shared.get() {
            return Ok(c.clone());
        }
        let c = self.net.client()?;
        Ok(self.shared.get_or_init(|| c).clone())
    }

    /// yt-dlp on one fixed identity of the current route (see `Net::pinned`).
    pub fn pinned_ytdlp(&self) -> Ytdlp {
        Ytdlp {
            bin: self.yt.bin.clone(),
            cache_dir: self.yt.cache_dir.clone(),
            net: self.net.pinned(),
            calls: Arc::clone(&self.yt.calls),
            quick_lookup: self.yt.quick_lookup,
        }
    }

    /// This runtime, with what only tests change carried over from `from`:
    /// where the feeds and thumbnails are fetched, how often a refresh
    /// saves, the pause between searches, and the test knobs of the route
    /// (see `Net::with_knobs_of`). A settings change builds a new runtime
    /// from the config, and a test's must keep using its local stand-ins
    /// instead of YouTube. In production these are constants, so this
    /// changes nothing there.
    pub fn carrying_test_bases(mut self, from: &Runtime) -> Self {
        self.feed_base.clone_from(&from.feed_base);
        self.thumb_base.clone_from(&from.thumb_base);
        self.save_every = from.save_every;
        self.pause = Arc::clone(&from.pause);
        self.net = self.net.with_knobs_of(&from.net);
        self.yt.net = self.yt.net.with_knobs_of(&from.net);
        self
    }

    /// Carries the registry of running yt-dlp calls over from `from`, whose
    /// route this one has: a settings change that keeps the route must not
    /// lose track of the calls still running under the old runtime.
    pub fn keeping_calls_of(mut self, from: &Runtime) -> Self {
        self.yt.calls = Arc::clone(&from.yt.calls);
        self
    }
}

/// The pause between a refresh's searches: `SEARCH_PAUSE`, except in tests.
/// The window's tests run the engine itself, and ask for none with
/// `QVEIL_TEST_NO_SEARCH_PAUSE=1`.
fn search_pause() -> Pause {
    if cfg!(test) || std::env::var_os("QVEIL_TEST_NO_SEARCH_PAUSE").is_some_and(|v| v == "1") {
        return Arc::new(|_| {});
    }
    Arc::new(|wanted| {
        use rand::Rng;
        let (low, high) = SEARCH_PAUSE;
        wait_while_wanted(rand::rng().random_range(low..=high), wanted)
    })
}

/// Waits `long`, or until `wanted` says no, which it asks every tenth of a
/// second: a client that hangs up, a route that is left or a setting turned
/// off ends the wait as soon as it would end a search.
fn wait_while_wanted(long: Duration, wanted: &dyn Fn() -> bool) {
    let until = Instant::now() + long;
    while wanted() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
}

/// Spaces out the searches of one refresh: each one after the first waits
/// for `Runtime::pause`. A search typed in waits for nothing.
pub(super) struct Pacing<'a> {
    pause: Option<&'a Pause>,
    sent: std::cell::Cell<bool>,
}

impl<'a> Pacing<'a> {
    pub(super) fn of(rt: &'a Runtime) -> Self {
        Pacing {
            pause: Some(&rt.pause),
            sent: Default::default(),
        }
    }

    pub(super) fn none() -> Self {
        Pacing {
            pause: None,
            sent: Default::default(),
        }
    }

    /// Before a search: waits, unless it is the first. Whether the search
    /// is still wanted.
    pub(super) fn before_search(&self, wanted: &dyn Fn() -> bool) -> bool {
        if self.sent.replace(true)
            && let Some(pause) = self.pause
        {
            pause(wanted);
        }
        wanted()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::testing::{TempDir, socks_spy};
    use std::time::{Duration, Instant};

    #[test]
    fn the_pause_ends_as_soon_as_it_is_not_wanted() {
        let started = Instant::now();
        wait_while_wanted(Duration::from_secs(60), &|| false);
        assert!(started.elapsed() < Duration::from_secs(30));
        let asked = std::cell::Cell::new(0);
        wait_while_wanted(Duration::from_secs(60), &|| {
            asked.set(asked.get() + 1);
            asked.get() < 3
        });
        assert!(started.elapsed() < Duration::from_secs(30));
        // And otherwise lasts as long as it was meant to.
        let started = Instant::now();
        wait_while_wanted(Duration::from_millis(50), &|| true);
        assert!(started.elapsed() >= Duration::from_millis(50));
        // The real one is a few seconds, which no two refreshes share.
        let (low, high) = SEARCH_PAUSE;
        assert!(Duration::from_secs(2) <= low && low < high && high <= Duration::from_secs(8));
    }

    #[test]
    fn short_requests_on_an_isolating_route_each_get_their_own_identity() {
        let dir = TempDir::new();
        let (port, users) = socks_spy();
        let cfg = Config {
            proxy: Some(format!("socks5h://127.0.0.1:{port}")),
            ..Config::default()
        };
        let rt = Runtime::new(cfg, &dir.paths(), None, false).unwrap();
        assert!(rt.net.isolates());
        for _ in 0..3 {
            let c = rt.web_client().unwrap();
            assert!(
                c.get("https://i.ytimg.com/vi/x/mqdefault.jpg")
                    .send()
                    .is_err()
            );
        }
        let users = users.lock().unwrap().clone();
        let distinct: std::collections::HashSet<_> = users.iter().collect();
        // With Tor, each would be a circuit of its own.
        assert_eq!((users.len(), distinct.len()), (3, 3), "{users:?}");
    }

    #[test]
    fn short_requests_on_other_routes_reuse_one_connection() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/x", listener.local_addr().unwrap());
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = std::sync::Arc::clone(&accepted);
        std::thread::spawn(move || {
            for c in listener.incoming().flatten() {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::spawn(move || {
                    let mut w = c.try_clone().unwrap();
                    let mut r = BufReader::new(c);
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        if line == "\r\n" {
                            let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
                        }
                    }
                });
            }
        });
        let dir = TempDir::new();
        let mut rt = Runtime::new(Config::default(), &dir.paths(), None, false).unwrap();
        rt.net = rt.net.allowing_http();
        for _ in 0..3 {
            let body = rt.web_client().unwrap().get(&url).send().unwrap().text();
            assert_eq!(body.unwrap(), "ok");
        }
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
