//! The feed of the channels you follow: refreshing it into the cache, and a
//! channel's own page.

use super::now;
use super::runtime::Runtime;
use super::subs::load_subs;
use crate::feed::{self, Entry, FeedCache};
use crate::ids;
use crate::store::{self, Paths};
use crate::subs::Sub;
use crate::util::lock;
use crate::video::Video;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::sync::Mutex;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

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
#[derive(Debug, Clone, PartialEq)]
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

impl Refresh {
    /// Whether a running refresh of this kind fetches all that `wanted` asks
    /// for, so that `wanted` can wait for its result instead of starting its
    /// own. Forcing covers what is stale; both cover asking for nothing.
    ///
    /// A refresh makes its list of channels when it starts, so one asked for
    /// by name, which may have been subscribed to since, is covered only by a
    /// refresh of that very channel, and not by one of all of them.
    pub fn covers(&self, wanted: &Refresh) -> bool {
        match (self, wanted) {
            (_, Refresh::Off) => true,
            (Refresh::Force, Refresh::Force | Refresh::Stale) => true,
            (Refresh::Stale, Refresh::Stale) => true,
            (Refresh::Channel(running), Refresh::Channel(wanted)) => running == wanted,
            _ => false,
        }
    }
}

/// What a refresh found out besides the videos, which are in the cache.
pub struct Refreshed {
    /// Channels whose RSS feed was down, so their dates are approximate.
    pub fallbacks: usize,
    pub errors: Vec<String>,
    /// Channels left out because they were fetched recently.
    pub fresh: usize,
}

/// Fetches what `refresh` asks for and saves it in the feed cache, a burst of
/// channels at a time (see `Saver`). `current` says whether `rt`'s route is
/// still the one in use, so a refresh that is running when the route changes
/// stops fetching (see `feed::fetch_all`). `progress(done, total)` is told
/// how it is going: once with nothing done, and after each channel.
pub fn refresh(
    rt: &Runtime,
    paths: &Paths,
    refresh: Refresh,
    current: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Refreshed> {
    let subs = load_subs(paths)?;
    let mut fresh = 0;
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
    let mut done = Refreshed {
        fallbacks: 0,
        errors: Vec::new(),
        fresh,
    };
    if due.is_empty() {
        return Ok(done);
    }
    // The slow part, with nothing locked: the network can take minutes over
    // Tor, and other commands must not wait for it. What is fetched is saved
    // as it arrives, if not channel by channel, and only for that moment is
    // the store locked, so a refresh that is cut off has kept nearly all it got.
    progress(0, due.len());
    let saver = Saver::new(paths, rt.save_every);
    let fetch = std::thread::scope(|scope| {
        // Something has to notice that what is waiting has waited long enough
        // when no further channel arrives to remind it, as when one is slow.
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let tick = (rt.save_every / 4).clamp(Duration::from_millis(5), Duration::from_millis(500));
        let saver = &saver;
        scope.spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(tick) {
                // A failure stays pending, and is tried again (and reported) by `finish`.
                let _ = saver.save_if_due();
            }
        });
        let fetch = feed::fetch_all(
            rt,
            &due,
            rt.cfg.feed_workers,
            current,
            &|f| saver.keep(f),
            progress,
        );
        drop(stop);
        fetch
    });
    done.fallbacks = fetch.fallbacks;
    done.errors = not_fetched(&fetch);
    done.errors.splice(0..0, fetch.errors);
    saver.finish()?;
    Ok(done)
}

/// Why the channels that a refresh didn't get to were left out, in words.
fn not_fetched(fetch: &feed::Fetch) -> Vec<String> {
    let mut said = Vec::new();
    if fetch.skipped > 0 {
        said.push(format!(
            "The route changed during the refresh; {} channel(s) were not fetched.",
            fetch.skipped
        ));
    }
    if fetch.unsaved > 0 {
        said.push(format!(
            "{} channel(s) were not fetched, because what was fetched couldn't be saved.",
            fetch.unsaved
        ));
    }
    said
}

/// The feed as the cache has it now, for a refresh that is over (or none).
pub fn feed_result(
    paths: &Paths,
    no_shorts: bool,
    limit: usize,
    done: &Refreshed,
) -> Result<FeedResult> {
    let subs = load_subs(paths)?;
    let cache: FeedCache = store::load_cache(&paths.feed_cache())?;
    Ok(FeedResult {
        entries: cache.entries(&subs.channels, no_shorts, limit),
        fallbacks: done.fallbacks,
        errors: done.errors.clone(),
        channels: subs.channels.len(),
        fresh: done.fresh,
    })
}

/// `refresh`, and then the feed.
pub fn feed(
    rt: &Runtime,
    paths: &Paths,
    refreshing: Refresh,
    no_shorts: bool,
    limit: usize,
    current: &(dyn Fn() -> bool + Sync),
) -> Result<FeedResult> {
    let done = refresh(rt, paths, refreshing, current, &|_, _| {})?;
    feed_result(paths, no_shorts, limit, &done)
}

/// Saves the channels that a refresh fetches, a batch at a time: a channel
/// is saved when it arrives only if the last save was long enough ago,
/// otherwise it waits (with the others that come meanwhile) for the next one.
struct Saver<'a> {
    paths: &'a Paths,
    every: Duration,
    waiting: Mutex<Waiting>,
    /// How many times the cache was written, for the tests.
    #[cfg(test)]
    saves: std::sync::atomic::AtomicUsize,
}

#[derive(Default)]
struct Waiting {
    /// Each with the time it arrived, which is the time it is dated by.
    fetched: Vec<(feed::Fetched, i64)>,
    last_save: Option<Instant>,
}

impl<'a> Saver<'a> {
    fn new(paths: &'a Paths, every: Duration) -> Self {
        Saver {
            paths,
            every,
            waiting: Mutex::default(),
            #[cfg(test)]
            saves: Default::default(),
        }
    }

    /// Takes a channel that was fetched. An error means it couldn't be saved
    /// (and neither could any that waited with it), which stops the refresh.
    fn keep(&self, f: feed::Fetched) -> Result<()> {
        let mut waiting = lock(&self.waiting);
        waiting.fetched.push((f, now()));
        if waiting.due(self.every) {
            self.save(&mut waiting)?;
        }
        Ok(())
    }

    fn save_if_due(&self) -> Result<()> {
        let mut waiting = lock(&self.waiting);
        if waiting.due(self.every) {
            self.save(&mut waiting)?;
        }
        Ok(())
    }

    /// The last save of a refresh, which happens even with nothing waiting:
    /// it rebuilds a cache that couldn't be read, and drops what is no longer
    /// subscribed to.
    fn finish(&self) -> Result<()> {
        self.save(&mut lock(&self.waiting))
    }

    fn save(&self, waiting: &mut Waiting) -> Result<()> {
        let batch = std::mem::take(&mut waiting.fetched);
        let saved = update_feed_cache(self.paths, |cache| {
            for (f, at) in &batch {
                cache.merge(std::slice::from_ref(f), *at);
            }
        });
        match saved {
            Ok(_) => {
                waiting.last_save = Some(Instant::now());
                #[cfg(test)]
                self.saves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
            Err(e) => {
                // Not lost: the next save has them too.
                waiting.fetched = batch;
                Err(e)
            }
        }
    }
}

impl Waiting {
    fn due(&self, every: Duration) -> bool {
        !self.fetched.is_empty() && self.last_save.is_none_or(|at| at.elapsed() >= every)
    }
}

/// Changes the feed cache as it is on disk now, not as it was when a refresh
/// began (another process may have refreshed in the meantime), and saves it.
fn update_feed_cache(paths: &Paths, change: impl FnOnce(&mut FeedCache)) -> Result<FeedCache> {
    let _lock = store::lock(paths)?;
    let mut cache: FeedCache = store::load_cache(&paths.feed_cache())?;
    change(&mut cache);
    // Channels that are no longer subscribed to have no use for a timestamp.
    // Read now rather than at the start of the refresh, which can be long
    // ago: a channel added since must keep its own.
    if let Ok(subs) = load_subs(paths) {
        cache
            .fetched_at
            .retain(|id, _| subs.channels.iter().any(|s| s.id == *id));
    }
    store::save_compact(&paths.feed_cache(), &cache)?;
    Ok(cache)
}

#[derive(Serialize)]
pub struct ChannelPage {
    pub id: String,
    pub name: String,
    pub subscribed: bool,
    pub videos: Vec<Entry>,
}

/// A channel's page. `id` is the channel's ID, or its `@handle` (how a link
/// in a description usually names a channel), which is looked up first.
pub fn channel(rt: &Runtime, paths: &Paths, id: &str) -> Result<ChannelPage> {
    let id = id.trim();
    let id = if ids::is_channel_id(id) {
        id.to_string()
    } else if let (true, Some(ids::ChannelRef::Url(url))) =
        (id.starts_with('@'), ids::parse_channel(id))
    {
        rt.yt.channel_info(&url)?.0
    } else {
        bail!("{id:?} is not a channel ID or a handle");
    };
    let id = id.as_str();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::now;
    use crate::app::runtime::Runtime;
    use crate::app::subs::{import_subs, load_subs};
    use crate::app::testing::{channel, offline, rt_cfg, with_feeds_at};
    use crate::config::Config;
    use crate::feed::{self, FeedCache};
    use crate::store::testing::{TempDir, listing_runs, listing_ytdlp};
    use crate::store::{self, Paths};
    use crate::util::lock;
    use crate::video::Video;
    use anyhow::bail;
    use std::fs;
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    #[test]
    fn a_running_refresh_covers_only_what_it_fetches() {
        let one = || Refresh::Channel(format!("UC{:022}", 1));
        let other = || Refresh::Channel(format!("UC{:022}", 2));
        // (running, wanted, covered)
        let table = [
            (Refresh::Force, Refresh::Force, true),
            (Refresh::Force, Refresh::Stale, true),
            (Refresh::Force, Refresh::Off, true),
            (Refresh::Stale, Refresh::Stale, true),
            (Refresh::Stale, Refresh::Off, true),
            (Refresh::Stale, Refresh::Force, false),
            (Refresh::Stale, one(), false),
            // Not even forcing: its list was made before this channel may have
            // been subscribed to.
            (Refresh::Force, one(), false),
            (one(), one(), true),
            (one(), other(), false),
            (one(), Refresh::Stale, false),
            (one(), Refresh::Force, false),
            (one(), Refresh::Off, true),
        ];
        for (running, wanted, covered) in table {
            assert_eq!(
                running.covers(&wanted),
                covered,
                "{running:?} running, {wanted:?} wanted"
            );
        }
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
        // The one channel that was started finished after the change, so what
        // it fetched came over the route that was left: it isn't kept either.
        assert!(r.entries.is_empty(), "{:?}", r.entries.len());
        assert_eq!(
            r.errors,
            ["The route changed during the refresh; 5 channel(s) were not fetched."]
        );
        let cache: FeedCache = store::load(&paths.feed_cache()).unwrap();
        assert!(cache.fetched_at.is_empty(), "all of them are still due");
    }

    #[test]
    fn channels_left_out_because_saving_failed_are_not_blamed_on_the_route() {
        let dir = TempDir::new();
        let paths = dir.paths();
        for n in 1..=3 {
            import_subs(&paths, &channel(n)).unwrap();
        }
        let ytdlp = listing_ytdlp(&dir.0, "");
        let rt = offline(&paths, &ytdlp);
        let subs = load_subs(&paths).unwrap().channels;
        // The disk is full: the first channel can't be kept, which stops the refresh.
        let fetch = feed::fetch_all(&rt, &subs, 1, &|| true, &|_| bail!("disk full"), &|_, _| {});
        assert_eq!((fetch.fetched, fetch.skipped, fetch.unsaved), (0, 0, 2));
        let said = not_fetched(&fetch);
        assert_eq!(
            said,
            ["2 channel(s) were not fetched, because what was fetched couldn't be saved."]
        );
        assert!(!said.iter().any(|s| s.contains("route")));
        // The one that was fetched says why it wasn't kept.
        assert_eq!(fetch.errors.len(), 1);
        assert!(fetch.errors[0].contains("disk full"), "{:?}", fetch.errors);
    }

    #[test]
    fn a_refresh_that_is_cut_off_has_kept_the_channels_it_got() {
        let dir = TempDir::new();
        let paths = dir.paths();
        for n in 1..=4 {
            import_subs(&paths, &channel(n)).unwrap();
        }
        // The third channel's lookup hangs for a while: the refresh is
        // "interrupted" there, as if the engine were killed.
        let runs = dir.0.join("runs");
        let ytdlp = listing_ytdlp(
            &dir.0,
            &format!(
                "if [ \"$(wc -l < '{}')\" -eq 3 ]; then sleep 2; fi",
                runs.display()
            ),
        );
        let rt = Runtime {
            cfg: Config {
                feed_workers: 1,
                ..rt_cfg(&ytdlp)
            },
            // Saves are batched (see `Saver`); here, a batch is a few ms.
            save_every: Duration::from_millis(50),
            ..offline(&paths, &ytdlp)
        };
        let refresh = {
            let paths = paths.clone();
            std::thread::spawn(move || feed(&rt, &paths, Refresh::Stale, false, 10, &|| true))
        };
        let saved =
            || store::load::<FeedCache>(&paths.feed_cache()).map_or(0, |c| c.fetched_at.len());
        let started = Instant::now();
        while saved() < 2 {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "nothing was saved while the refresh was running"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !refresh.is_finished(),
            "the first two must be on disk before the refresh is over"
        );
        let cache: FeedCache = store::load(&paths.feed_cache()).unwrap();
        assert_eq!(
            cache.seen.len(),
            2,
            "their videos too, not just the timestamps"
        );

        let r = refresh.join().unwrap().unwrap();
        assert_eq!((r.entries.len(), saved(), listing_runs(&dir.0)), (4, 4, 4));
    }

    #[test]
    fn rss_is_left_out_for_the_rest_of_a_refresh_once_it_keeps_failing() {
        let dir = TempDir::new();
        let paths = dir.paths();
        for n in 1..=8 {
            import_subs(&paths, &channel(n)).unwrap();
        }
        // An RSS endpoint that is down, and a fallback that works.
        let (base, rss_requests) = crate::store::testing::rss_upstream(usize::MAX);
        let ytdlp = listing_ytdlp(&dir.0, "");
        let rt = Runtime {
            cfg: Config {
                feed_workers: 1,
                ..rt_cfg(&ytdlp)
            },
            ..with_feeds_at(&paths, &base, &ytdlp)
        };
        let r = feed(&rt, &paths, Refresh::Force, false, 20, &|| true).unwrap();
        // Three channels asked RSS twice each; the other five went straight
        // to the fallback.
        assert_eq!(rss_requests.load(Ordering::SeqCst), 6);
        assert_eq!(listing_runs(&dir.0), 8);
        assert_eq!((r.fallbacks, r.errors.len(), r.entries.len()), (8, 0, 8));

        // The next refresh starts over: RSS is asked again, as it may be up.
        feed(&rt, &paths, Refresh::Force, false, 20, &|| true).unwrap();
        assert_eq!(rss_requests.load(Ordering::SeqCst), 12);
    }

    #[test]
    fn a_working_rss_is_used_for_every_channel_and_needs_no_fallback() {
        let dir = TempDir::new();
        let paths = dir.paths();
        for n in 1..=4 {
            import_subs(&paths, &channel(n)).unwrap();
        }
        // The first two requests fail (the first channel's), then it recovers.
        let (base, rss_requests) = crate::store::testing::rss_upstream(2);
        let ytdlp = listing_ytdlp(&dir.0, "");
        let rt = Runtime {
            cfg: Config {
                feed_workers: 1,
                ..rt_cfg(&ytdlp)
            },
            ..with_feeds_at(&paths, &base, &ytdlp)
        };
        let r = feed(&rt, &paths, Refresh::Force, false, 20, &|| true).unwrap();
        assert_eq!(rss_requests.load(Ordering::SeqCst), 5, "2 + 1 + 1 + 1");
        assert_eq!((r.fallbacks, listing_runs(&dir.0)), (1, 1));
        assert_eq!(r.entries.len(), 4);
    }

    #[test]
    fn an_rss_reply_that_arrives_after_the_route_changed_is_dropped() {
        let dir = TempDir::new();
        let paths = dir.paths();
        for n in 1..=3 {
            import_subs(&paths, &channel(n)).unwrap();
        }
        let (base, rss_requests) = crate::store::testing::rss_upstream(0);
        let ytdlp = listing_ytdlp(&dir.0, "");
        let rt = Runtime {
            cfg: Config {
                feed_workers: 1,
                ..rt_cfg(&ytdlp)
            },
            ..with_feeds_at(&paths, &base, &ytdlp)
        };
        // "The route changes" while the first reply is on its way: it is
        // current until the upstream has been asked.
        let current = || rss_requests.load(Ordering::SeqCst) == 0;
        let r = feed(&rt, &paths, Refresh::Stale, false, 20, &current).unwrap();
        assert_eq!(rss_requests.load(Ordering::SeqCst), 1);
        assert!(r.entries.is_empty(), "the late reply was not used");
        assert_eq!(
            r.errors,
            ["The route changed during the refresh; 3 channel(s) were not fetched."]
        );
        let cache: FeedCache = store::load(&paths.feed_cache()).unwrap();
        assert!(cache.fetched_at.is_empty() && cache.seen.is_empty());
        assert_eq!(listing_runs(&dir.0), 0);
    }

    fn fetched(channel: u32, video: &str) -> feed::Fetched {
        feed::Fetched {
            channel_id: format!("UC{channel:022}"),
            videos: vec![Video {
                id: video.into(),
                title: video.into(),
                channel: None,
                channel_id: Some(format!("UC{channel:022}")),
                published: Some(1_700_000_000),
                duration: None,
                views: None,
                short: false,
                live: false,
            }],
            source: feed::Source::Rss,
        }
    }

    #[test]
    fn a_burst_of_channels_shares_one_write_of_the_cache() {
        let dir = TempDir::new();
        let paths = dir.paths();
        for n in 1..=10 {
            import_subs(&paths, &channel(n)).unwrap();
        }
        let saves = |saver: &Saver| saver.saves.load(Ordering::SeqCst);
        let saver = Saver::new(&paths, Duration::from_secs(3600));
        for n in 1..=10 {
            saver.keep(fetched(n, &format!("video{n:06}"))).unwrap();
        }
        // The first channel is saved at once, the other nine wait together…
        assert_eq!(saves(&saver), 1);
        saver.save_if_due().unwrap();
        assert_eq!(saves(&saver), 1, "an hour hasn't passed");
        let cache: FeedCache = store::load(&paths.feed_cache()).unwrap();
        assert_eq!(cache.seen.len(), 1);
        // …and the end of the refresh saves what is left.
        saver.finish().unwrap();
        assert_eq!(saves(&saver), 2);
        let cache: FeedCache = store::load(&paths.feed_cache()).unwrap();
        assert_eq!((cache.seen.len(), cache.fetched_at.len()), (10, 10));

        // With no wait to speak of, every channel is its own write.
        let eager = Saver::new(&paths, Duration::ZERO);
        for n in 1..=3 {
            eager.keep(fetched(n, &format!("other{n:07}"))).unwrap();
        }
        assert_eq!(saves(&eager), 3);
    }

    #[test]
    fn what_could_not_be_saved_is_kept_for_the_next_save() {
        let dir = TempDir::new();
        let paths = dir.paths();
        import_subs(&paths, &channel(1)).unwrap();
        // A directory where the cache file goes: the write fails.
        fs::create_dir_all(paths.feed_cache()).unwrap();
        let saver = Saver::new(&paths, Duration::ZERO);
        assert!(saver.keep(fetched(1, "videoAAAAAA")).is_err());
        assert!(saver.finish().is_err());
        fs::remove_dir_all(paths.feed_cache()).unwrap();
        saver.finish().unwrap();
        let cache: FeedCache = store::load(&paths.feed_cache()).unwrap();
        assert!(
            cache.seen.contains_key("videoAAAAAA"),
            "the channel was not lost"
        );
    }

    #[test]
    fn the_cache_is_written_as_compact_json() {
        let dir = TempDir::new();
        let paths = dir.paths();
        import_subs(&paths, &channel(1)).unwrap();
        let rt = offline(&paths, &listing_ytdlp(&dir.0, ""));
        feed(&rt, &paths, Refresh::Stale, false, 10, &|| true).unwrap();
        let text = fs::read_to_string(paths.feed_cache()).unwrap();
        assert!(
            text.starts_with("{\"seen\":") && !text.contains('\n'),
            "{text}"
        );
        // The user's own data stays easy to read and edit.
        import_subs(&paths, &channel(2)).unwrap();
        assert!(
            fs::read_to_string(paths.subscriptions())
                .unwrap()
                .contains("\n  ")
        );
    }

    #[test]
    fn a_refresh_reports_its_progress_one_channel_at_a_time() {
        let dir = TempDir::new();
        let paths = dir.paths();
        for n in 1..=4 {
            import_subs(&paths, &channel(n)).unwrap();
        }
        let ytdlp = listing_ytdlp(&dir.0, "");
        let rt = Runtime {
            cfg: Config {
                feed_workers: 3,
                ..rt_cfg(&ytdlp)
            },
            ..offline(&paths, &ytdlp)
        };
        let told = Mutex::new(Vec::new());
        refresh(&rt, &paths, Refresh::Stale, &|| true, &|done, total| {
            lock(&told).push((done, total))
        })
        .unwrap();
        // In order, whichever worker finished: the window shows a bar.
        assert_eq!(*lock(&told), [(0, 4), (1, 4), (2, 4), (3, 4), (4, 4)]);

        // Nothing to fetch, nothing to report.
        let told = Mutex::new(Vec::new());
        refresh(&rt, &paths, Refresh::Stale, &|| true, &|done, total| {
            lock(&told).push((done, total))
        })
        .unwrap();
        assert!(lock(&told).is_empty());
    }
}
