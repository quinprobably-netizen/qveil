//! What qveil can do, shared by the command line and the local server that the
//! desktop app talks to.

use crate::config::Config;
use crate::feed::{self, Entry, FeedCache, Source};
use crate::ids;
use crate::net::{self, Net};
use crate::store::{self, Paths};
use crate::subs::{self, Sub, Subs};
use crate::video::Video;
use crate::ytdlp::Ytdlp;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub struct Runtime {
    pub cfg: Config,
    pub net: Net,
    pub yt: Ytdlp,
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
        };
        Ok(Self { cfg, net, yt })
    }

    /// yt-dlp on one fixed identity of the current route (see `Net::pinned`).
    pub fn pinned_ytdlp(&self) -> Ytdlp {
        Ytdlp {
            bin: self.yt.bin.clone(),
            cache_dir: self.yt.cache_dir.clone(),
            net: self.net.pinned(),
        }
    }
}

pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

pub fn search(rt: &Runtime, query: &str, limit: usize) -> Result<Vec<Video>> {
    let query = query.trim();
    if query.is_empty() {
        bail!("empty search");
    }
    rt.yt.search(query, limit)
}

#[derive(Serialize)]
pub struct FeedResult {
    pub entries: Vec<Entry>,
    /// Channels whose RSS feed was down, so their dates are approximate.
    pub fallbacks: usize,
    pub errors: Vec<String>,
    pub channels: usize,
    /// Channels left out of this refresh because they were fetched recently.
    /// For the command line; the API's reply doesn't carry it.
    #[serde(skip)]
    pub fresh: usize,
}

/// What a feed request fetches before it answers from the cache.
#[derive(Debug, PartialEq)]
pub enum Refresh {
    /// Nothing: show what is cached.
    Off,
    /// Channels not fetched within `feed::FRESH_SECS`.
    Stale,
    /// Every channel.
    Force,
    /// Just this one, whatever its age.
    Channel(String),
}

/// `current` says whether `rt`'s route is still the one in use, so a refresh
/// that is running when the route changes stops fetching (see `feed::fetch_all`).
pub fn feed(
    rt: &Runtime,
    paths: &Paths,
    refresh: Refresh,
    no_shorts: bool,
    limit: usize,
    current: &(dyn Fn() -> bool + Sync),
) -> Result<FeedResult> {
    let subs = load_subs(paths)?;
    let (mut fallbacks, mut errors, mut fresh) = (0, Vec::new(), 0);
    let due: Vec<Sub> = match &refresh {
        Refresh::Off => vec![],
        Refresh::Force => subs.channels.clone(),
        Refresh::Channel(id) => {
            if !ids::is_channel_id(id) {
                bail!("{id:?} is not a channel ID");
            }
            let sub = subs.channels.iter().find(|s| s.id == *id);
            vec![
                sub.with_context(|| format!("{id} is not one of your subscriptions"))?
                    .clone(),
            ]
        }
        Refresh::Stale => {
            let cache: FeedCache = store::load_cache(&paths.feed_cache())?;
            let t = now();
            let (due, kept): (Vec<_>, Vec<_>) = subs
                .channels
                .iter()
                .cloned()
                .partition(|s| cache.is_stale(&s.id, t));
            fresh = kept.len();
            due
        }
    };
    let cache: FeedCache = if !due.is_empty() {
        // The slow part, with nothing locked: the network can take minutes
        // over Tor, and other commands must not wait for it.
        let fetch = feed::fetch_all(&rt.net, &rt.yt, &due, rt.cfg.feed_workers, current);
        fallbacks = fetch
            .fetched
            .iter()
            .filter(|f| f.source == Source::Fallback)
            .count();
        errors = fetch.errors;
        if fetch.skipped > 0 {
            errors.push(format!(
                "The route changed during the refresh; {} channel(s) were not fetched.",
                fetch.skipped
            ));
        }
        // Merge into what is on disk now, not what was there when this
        // started: another process may have refreshed in the meantime.
        let _lock = store::lock(paths)?;
        let mut cache: FeedCache = store::load_cache(&paths.feed_cache())?;
        cache.merge(&fetch.fetched, now());
        // Channels that are no longer subscribed to have no use for a timestamp.
        cache
            .fetched_at
            .retain(|id, _| subs.channels.iter().any(|s| s.id == *id));
        store::save(&paths.feed_cache(), &cache)?;
        cache
    } else {
        store::load_cache(&paths.feed_cache())?
    };
    Ok(FeedResult {
        entries: cache.entries(&subs.channels, no_shorts, limit),
        fallbacks,
        errors,
        channels: subs.channels.len(),
        fresh,
    })
}

#[derive(Serialize)]
pub struct ChannelPage {
    pub id: String,
    pub name: String,
    pub subscribed: bool,
    pub videos: Vec<Entry>,
}

pub fn channel(rt: &Runtime, paths: &Paths, id: &str) -> Result<ChannelPage> {
    if !ids::is_channel_id(id) {
        bail!("{id:?} is not a channel ID");
    }
    let videos = rt.yt.channel_videos(id, 30)?;
    let sub = load_subs(paths)?.channels.into_iter().find(|s| s.id == id);
    let name = sub
        .as_ref()
        .map(|s| s.name.clone())
        .or_else(|| videos.iter().find_map(|v| v.channel.clone()))
        .unwrap_or_else(|| id.to_string());
    Ok(ChannelPage {
        id: id.into(),
        name,
        subscribed: sub.is_some(),
        // The Videos tab only gives "3 days ago", so every date is approximate.
        videos: videos
            .into_iter()
            .map(|v| Entry {
                date: v.published.unwrap_or(0),
                estimated: true,
                video: Video {
                    published: None,
                    ..v
                },
            })
            .collect(),
    })
}

pub fn load_subs(paths: &Paths) -> Result<Subs> {
    store::load(&paths.subscriptions())
}

/// Returns the subscription and whether it is new.
pub fn add_sub(rt: &Runtime, paths: &Paths, input: &str) -> Result<(Sub, bool)> {
    let r = ids::parse_channel(input)
        .with_context(|| format!("{input:?} isn't a channel (@handle, channel URL or UC… ID)"))?;
    let (id, name) = rt.yt.channel_info(&r.url())?;
    // Only the file update is locked, not the lookup above.
    let _lock = store::lock(paths)?;
    let mut subs = load_subs(paths)?;
    let sub = Sub { id, name };
    let added = subs.add(sub.clone());
    store::save(&paths.subscriptions(), &subs)?;
    Ok((sub, added))
}

pub fn remove_sub(paths: &Paths, query: &str) -> Result<Sub> {
    let _lock = store::lock(paths)?;
    let mut subs = load_subs(paths)?;
    let removed = subs
        .remove(query)
        .with_context(|| format!("no subscription matches {query:?}"))?;
    store::save(&paths.subscriptions(), &subs)?;
    Ok(removed)
}

/// Returns (newly added, found in the file). Makes no network requests.
pub fn import_subs(paths: &Paths, text: &str) -> Result<(usize, usize)> {
    let found = subs::parse_import(text)?;
    let _lock = store::lock(paths)?;
    let mut subs = load_subs(paths)?;
    let total = found.len();
    let added = found.into_iter().filter(|s| subs.add(s.clone())).count();
    store::save(&paths.subscriptions(), &subs)?;
    Ok((added, total))
}

#[derive(Default, Serialize, Deserialize)]
pub struct History {
    pub watched: Vec<Watched>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Watched {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    pub at: i64,
}

const HISTORY_CAP: usize = 5000;

pub fn load_history(paths: &Paths) -> Result<History> {
    store::load(&paths.history())
}

pub fn record_history(paths: &Paths, w: Watched) -> Result<()> {
    let _lock = store::lock(paths)?;
    let mut h = load_history(paths)?;
    h.watched.retain(|x| x.id != w.id);
    h.watched.push(w);
    let excess = h.watched.len().saturating_sub(HISTORY_CAP);
    h.watched.drain(..excess);
    store::save(&paths.history(), &h)
}

pub fn clear_history(paths: &Paths) -> Result<()> {
    let _lock = store::lock(paths)?;
    match std::fs::remove_file(paths.history()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{TempDir, listing_runs, listing_ytdlp};
    use std::fs;
    use std::time::{Duration, Instant};

    /// A runtime whose every request fails at once (nothing listens on the
    /// proxy port) and whose yt-dlp is `ytdlp`.
    fn offline(paths: &Paths, ytdlp: &str) -> Runtime {
        let cfg = Config {
            ytdlp: ytdlp.into(),
            ..Config::default()
        };
        Runtime::new(cfg, paths, Some("http://127.0.0.1:1".into()), false).unwrap()
    }

    fn channel(n: u32) -> String {
        format!("UC{n:022},http://x,Chan{n}\n")
    }

    #[test]
    fn a_corrupt_feed_cache_is_rebuilt_instead_of_failing() {
        let dir = TempDir::new();
        let paths = dir.paths();
        import_subs(&paths, &channel(1)).unwrap();
        fs::create_dir_all(&paths.cache).unwrap();
        for broken in ["", "{\"seen\":{\"abc\":{\"video\":", "garbage"] {
            fs::write(paths.feed_cache(), broken).unwrap();
            let rt = offline(&paths, "false");

            // Offline: the cache reads as empty.
            let r = feed(&rt, &paths, Refresh::Off, false, 10, &|| true).unwrap();
            assert_eq!((r.channels, r.entries.len()), (1, 0));

            // Refreshing: every fetch fails here, but the cache is rewritten as valid.
            let r = feed(&rt, &paths, Refresh::Stale, false, 10, &|| true).unwrap();
            assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
            let _: FeedCache = store::load(&paths.feed_cache()).unwrap();
        }
    }

    #[test]
    fn corrupt_subscriptions_are_an_error_and_are_left_alone() {
        let dir = TempDir::new();
        let paths = dir.paths();
        fs::create_dir_all(&paths.data).unwrap();
        fs::write(paths.subscriptions(), "{\"channels\":[{\"id\":").unwrap();
        assert!(load_subs(&paths).is_err());
        assert!(import_subs(&paths, &channel(1)).is_err());
        assert!(remove_sub(&paths, "Chan1").is_err());
        assert_eq!(
            fs::read_to_string(paths.subscriptions()).unwrap(),
            "{\"channels\":[{\"id\":"
        );
    }

    #[test]
    fn simultaneous_changes_are_all_kept() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let paths = paths.clone();
                std::thread::spawn(move || {
                    for k in 0..5 {
                        import_subs(&paths, &channel(n * 10 + k)).unwrap();
                        record_history(
                            &paths,
                            Watched {
                                id: format!("v{n:02}{k:08}"),
                                title: "t".into(),
                                channel: None,
                                at: 0,
                            },
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(load_subs(&paths).unwrap().channels.len(), 40);
        assert_eq!(load_history(&paths).unwrap().watched.len(), 40);
    }

    #[test]
    fn the_store_is_not_locked_while_feeds_are_being_fetched() {
        let dir = TempDir::new();
        let paths = dir.paths();
        import_subs(&paths, &channel(1)).unwrap();
        // A yt-dlp that takes a couple of seconds and then fails.
        let slow = crate::store::testing::script(&dir.0.join("slow-yt-dlp"), "sleep 2\nexit 1\n");
        let rt = offline(&paths, &slow);

        let refresh = {
            let paths = paths.clone();
            std::thread::spawn(move || {
                feed(&rt, &paths, Refresh::Stale, false, 10, &|| true).unwrap()
            })
        };
        std::thread::sleep(Duration::from_millis(500));
        // `try_lock` rather than `lock`, so a regression fails instead of hanging.
        let file = fs::File::open(paths.lock_file()).unwrap();
        let started = Instant::now();
        while file.try_lock().is_err() {
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "the store stayed locked while feeds were being fetched"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(file);
        // And the rest of the app keeps working meanwhile.
        import_subs(&paths, &channel(2)).unwrap();
        let r = refresh.join().unwrap();
        assert_eq!(r.errors.len(), 1);
        assert_eq!(load_subs(&paths).unwrap().channels.len(), 2);
    }

    fn cached_at(paths: &Paths, ids: &[(u32, i64)]) {
        let mut cache = FeedCache::default();
        for (n, at) in ids {
            cache.fetched_at.insert(format!("UC{n:022}"), *at);
        }
        store::save(&paths.feed_cache(), &cache).unwrap();
    }

    #[test]
    fn a_refresh_leaves_out_channels_fetched_recently() {
        let dir = TempDir::new();
        let paths = dir.paths();
        for n in 1..=3 {
            import_subs(&paths, &channel(n)).unwrap();
        }
        let rt = offline(&paths, &listing_ytdlp(&dir.0, ""));
        let t = now();
        // 1 was fetched a minute ago, 2 an hour ago, and 3 never.
        cached_at(&paths, &[(1, t - 60), (2, t - 3600)]);
        let go = |refresh| feed(&rt, &paths, refresh, false, 10, &|| true).unwrap();

        let r = go(Refresh::Stale);
        assert_eq!((listing_runs(&dir.0), r.fresh, r.errors.len()), (2, 1, 0));
        // Now all three are fresh, so nothing is fetched at all.
        let r = go(Refresh::Stale);
        assert_eq!((listing_runs(&dir.0), r.fresh), (2, 3));
        assert_eq!(r.entries.len(), 2, "the cache is still shown");
        // Not refreshing never fetches, and force fetches all.
        go(Refresh::Off);
        assert_eq!(listing_runs(&dir.0), 2);
        let r = go(Refresh::Force);
        assert_eq!((listing_runs(&dir.0), r.fresh), (5, 0));
        // One channel is fetched whatever its age.
        go(Refresh::Channel(format!("UC{:022}", 2)));
        assert_eq!(listing_runs(&dir.0), 6);
        let cache: FeedCache = store::load(&paths.feed_cache()).unwrap();
        assert_eq!(cache.fetched_at.len(), 3);
    }

    #[test]
    fn a_failed_channel_is_tried_again_next_time() {
        let dir = TempDir::new();
        let paths = dir.paths();
        import_subs(&paths, &channel(1)).unwrap();
        let rt = offline(&paths, "false");
        let r = feed(&rt, &paths, Refresh::Stale, false, 10, &|| true).unwrap();
        assert_eq!(r.errors.len(), 1);
        let cache: FeedCache = store::load(&paths.feed_cache()).unwrap();
        assert!(cache.fetched_at.is_empty(), "a failure is not a fetch");
        let r = feed(&rt, &paths, Refresh::Stale, false, 10, &|| true).unwrap();
        assert_eq!((r.errors.len(), r.fresh), (1, 0));
    }

    #[test]
    fn a_single_channel_refresh_must_name_a_subscription() {
        let dir = TempDir::new();
        let paths = dir.paths();
        import_subs(&paths, &channel(1)).unwrap();
        let rt = offline(&paths, &listing_ytdlp(&dir.0, ""));
        for (id, said) in [
            ("not-an-id".to_string(), "not a channel ID"),
            (format!("UC{:022}", 9), "not one of your subscriptions"),
        ] {
            let err = feed(&rt, &paths, Refresh::Channel(id), false, 10, &|| true)
                .err()
                .expect("must refuse");
            assert!(err.to_string().contains(said), "{err}");
        }
        assert_eq!(listing_runs(&dir.0), 0);
    }

    #[test]
    fn a_refresh_stops_fetching_once_the_route_has_changed() {
        let dir = TempDir::new();
        let paths = dir.paths();
        for n in 1..=5 {
            import_subs(&paths, &channel(n)).unwrap();
        }
        // The first channel's fetch is what "changes the route" (it creates
        // the file `current` looks for), and one worker takes the channels in turn.
        let moved = dir.0.join("moved");
        let ytdlp = listing_ytdlp(&dir.0, &format!("touch '{}'", moved.display()));
        let rt = Runtime {
            cfg: Config {
                feed_workers: 1,
                ..rt_cfg(&ytdlp)
            },
            ..offline(&paths, &ytdlp)
        };
        let r = feed(&rt, &paths, Refresh::Stale, false, 10, &|| !moved.exists()).unwrap();
        assert_eq!(
            listing_runs(&dir.0),
            1,
            "no channel was started after the change"
        );
        assert_eq!(r.entries.len(), 1, "what was fetched is kept");
        assert_eq!(
            r.errors,
            ["The route changed during the refresh; 4 channel(s) were not fetched."]
        );
        let cache: FeedCache = store::load(&paths.feed_cache()).unwrap();
        assert_eq!(cache.fetched_at.len(), 1, "the others are still due");
    }

    fn rt_cfg(ytdlp: &str) -> Config {
        Config {
            ytdlp: ytdlp.into(),
            ..Config::default()
        }
    }
}
