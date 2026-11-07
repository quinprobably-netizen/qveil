//! What veil can do, shared by the command line and the local server that the
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
}

pub fn feed(
    rt: &Runtime,
    paths: &Paths,
    refresh: bool,
    no_shorts: bool,
    limit: usize,
) -> Result<FeedResult> {
    let subs = load_subs(paths)?;
    let (mut fallbacks, mut errors) = (0, Vec::new());
    let cache: FeedCache = if refresh && !subs.channels.is_empty() {
        // The slow part, with nothing locked: the network can take minutes
        // over Tor, and other commands must not wait for it.
        let (fetched, errs) = feed::fetch_all(&rt.net, &rt.yt, &subs.channels, rt.cfg.feed_workers);
        fallbacks = fetched
            .iter()
            .filter(|f| f.source == Source::Fallback)
            .count();
        errors = errs;
        // Merge into what is on disk now, not what was there when this
        // started: another process may have refreshed in the meantime.
        let _lock = store::lock(paths)?;
        let mut cache: FeedCache = store::load_cache(&paths.feed_cache())?;
        cache.merge(&fetched, now());
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
    use crate::store::testing::TempDir;
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
            let r = feed(&rt, &paths, false, false, 10).unwrap();
            assert_eq!((r.channels, r.entries.len()), (1, 0));

            // Refreshing: every fetch fails here, but the cache is rewritten as valid.
            let r = feed(&rt, &paths, true, false, 10).unwrap();
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
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new();
        let paths = dir.paths();
        import_subs(&paths, &channel(1)).unwrap();
        // A yt-dlp that takes a couple of seconds and then fails.
        let slow = dir.0.join("slow-yt-dlp");
        fs::write(&slow, "#!/bin/sh\nsleep 2\nexit 1\n").unwrap();
        fs::set_permissions(&slow, fs::Permissions::from_mode(0o755)).unwrap();
        let rt = offline(&paths, slow.to_str().unwrap());

        let refresh = {
            let paths = paths.clone();
            std::thread::spawn(move || feed(&rt, &paths, true, false, 10).unwrap())
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
}
