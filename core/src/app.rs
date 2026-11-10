//! What qveil can do, shared by the command line and the local server that the
//! desktop app talks to.

use crate::config::Config;
use crate::feed::{self, Entry, FeedCache};
use crate::ids;
use crate::lens::rank::{self, Found, Ranked};
use crate::lens::read::{Reading, Tag, read};
use crate::lens::{self, Cached, Lens};
use crate::net::{self, Net};
use crate::store::{self, Paths};
use crate::subs::{self, Sub, Subs};
use crate::topics;
use crate::util::lock;
use crate::video::Video;
use crate::ytdlp::Ytdlp;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long what a refresh has fetched may wait before it is saved. Saving
/// rewrites the whole cache, which for a few hundred channels is a lot to do
/// after each one; a burst of channels shares one write, and an engine that is
/// killed mid-refresh loses at most this much of it.
const SAVE_EVERY: Duration = Duration::from_secs(2);

pub struct Runtime {
    pub cfg: Config,
    pub net: Net,
    pub yt: Ytdlp,
    /// The client short requests share where the route allows it (see `web_client`).
    shared: OnceLock<Client>,
    /// Where the engine's own requests go: the channel feeds and the
    /// thumbnails. Only tests change them, to stand a local server in for YouTube.
    pub feed_base: String,
    pub thumb_base: String,
    /// How often a refresh saves (`SAVE_EVERY`); only tests change it.
    pub save_every: Duration,
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
        };
        Ok(Self {
            cfg,
            net,
            yt,
            shared: OnceLock::new(),
            feed_base: "https://www.youtube.com".into(),
            thumb_base: "https://i.ytimg.com".into(),
            save_every: SAVE_EVERY,
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
        }
    }

    /// Carries the registry of running yt-dlp calls over from `from`, whose
    /// route this one has: a settings change that keeps the route must not
    /// lose track of the calls still running under the old runtime.
    pub fn keeping_calls_of(mut self, from: &Runtime) -> Self {
        self.yt.calls = Arc::clone(&from.yt.calls);
        self
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

/// `search`, with each result handed to `each` as soon as it is in (see
/// `Ytdlp::search_each`).
pub fn search_each(
    rt: &Runtime,
    query: &str,
    limit: usize,
    each: &mut dyn FnMut(Video) -> bool,
) -> Result<usize> {
    let query = query.trim();
    if query.is_empty() {
        bail!("empty search");
    }
    rt.yt.search_each(query, limit, None, each)
}

/// Whether a lens's searches are run before it is answered.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LensRefresh {
    /// Only what is stored.
    Off,
    /// When there are no results yet, or they are older than `lens::STALE`.
    Stale,
    Force,
}

/// A lens with how it was read and what its searches found.
#[derive(Serialize)]
pub struct LensResult {
    pub lens: Lens,
    pub tags: Vec<Tag>,
    /// What a refresh asks YouTube for, word for word.
    pub searches: Vec<String>,
    #[serde(rename = "fetchedAt", skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    pub videos: Vec<Ranked>,
}

/// The lens `id`, fetched again if `refresh` says so. `progress` hears how
/// many of its searches are done, and `wanted` is asked between them (and
/// while one runs): once nobody wants the answer, or the route it runs on has
/// been left, it stops and nothing is saved.
pub fn lens(
    rt: &Runtime,
    paths: &Paths,
    id: &str,
    refresh: LensRefresh,
    progress: &dyn Fn(usize, usize),
    wanted: &dyn Fn() -> bool,
) -> Result<LensResult> {
    let lens = lens::get(paths, id)?;
    let at = now();
    let reading = read(&lens.text, at);
    let stored = lens::cached(paths, id)?;
    let due = match refresh {
        LensRefresh::Off => false,
        LensRefresh::Stale => stored
            .as_ref()
            .is_none_or(|c| at - c.fetched_at >= lens::STALE),
        LensRefresh::Force => true,
    };
    let stored = if due {
        let videos = search_topics(rt, paths, &reading, lens::PER_TOPIC, progress, wanted)?;
        let fresh = Cached {
            fetched_at: at,
            videos,
        };
        // Asked once more, as late as it can be: the searches took a while.
        if !wanted() {
            bail!(crate::ytdlp::CANCELLED);
        }
        if !lens::save_results(paths, &lens, &fresh)? {
            bail!("That lens was changed while it was being refreshed; open it again.");
        }
        Some(fresh)
    } else {
        stored
    };
    let (fetched_at, videos) = match stored {
        Some(c) => (Some(c.fetched_at), c.videos),
        None => (None, Vec::new()),
    };
    Ok(LensResult {
        tags: reading.tags(),
        searches: reading.topics,
        lens,
        fetched_at,
        videos,
    })
}

/// A search whose query had rules in it: its topics are searched, and what
/// comes back is filtered and ordered here, like a lens that isn't kept.
pub fn smart_search(
    rt: &Runtime,
    paths: &Paths,
    reading: &Reading,
    limit: usize,
    wanted: &dyn Fn() -> bool,
) -> Result<Vec<Ranked>> {
    search_topics(rt, paths, reading, limit, &|_, _| {}, wanted)
}

/// One search per topic, one after the other, then the ranking. The searches
/// are all that leaves the machine: the rules and the order are applied here.
fn search_topics(
    rt: &Runtime,
    paths: &Paths,
    reading: &Reading,
    per_topic: usize,
    progress: &dyn Fn(usize, usize),
    wanted: &dyn Fn() -> bool,
) -> Result<Vec<Ranked>> {
    reading.check()?;
    let total = reading.topics.len();
    progress(0, total);
    // Where YouTube has a filter that contains a rule, it does the first
    // pass; the rules are applied here afterwards either way.
    let filter = lens::sp::sp(reading, now());
    let mut found = Vec::new();
    for (topic, query) in reading.topics.iter().enumerate() {
        let mut search = |sp: Option<&str>| {
            let mut rank = 0;
            rt.yt.search_each(query, per_topic, sp, &mut |video| {
                found.push(Found { video, topic, rank });
                rank += 1;
                wanted()
            })
        };
        // The filter is undocumented and may stop working. If it fails or
        // finds nothing, the topic is searched once more without it.
        let filtered = match filter.as_deref() {
            Some(sp) => matches!(search(Some(sp)), Ok(n) if n > 0),
            None => false,
        };
        if !filtered && wanted() {
            search(None)?;
        }
        if !wanted() {
            bail!(crate::ytdlp::CANCELLED);
        }
        progress(topic + 1, total);
    }
    let subscribed: HashSet<String> = load_subs(paths)?
        .channels
        .into_iter()
        .map(|c| c.id)
        .collect();
    // What was watched is only known when the history is kept.
    let watched: HashSet<String> = if rt.cfg.history {
        load_history(paths)?
            .watched
            .into_iter()
            .map(|w| w.id)
            .collect()
    } else {
        HashSet::new()
    };
    Ok(rank::rank(
        reading,
        found,
        &rank::Context {
            subscribed: &subscribed,
            watched: &watched,
            now: now(),
        },
    ))
}

/// A topic feed: a group of the channels you follow, and what its searches
/// last found on channels you don't.
#[derive(Serialize)]
pub struct TopicFeed {
    #[serde(flatten)]
    pub group: topics::Group,
    #[serde(rename = "fetchedAt", skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    pub videos: Vec<Ranked>,
}

#[derive(Serialize)]
pub struct Topics {
    /// Whether topic feeds are on. Off, there are no groups.
    pub enabled: bool,
    pub groups: Vec<TopicFeed>,
    /// The groups whose searches failed this time, in words.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// The groups the followed channels fall into now, worked out from what is on
/// this machine (the subscriptions and the titles in the feed cache).
fn topic_groups(paths: &Paths) -> Result<(Vec<topics::Group>, HashSet<String>)> {
    let subs = load_subs(paths)?;
    let cache: FeedCache = store::load_cache(&paths.feed_cache())?;
    // Each channel's titles, newest first.
    let mut latest: Vec<&feed::Seen> = cache.seen.values().collect();
    latest.sort_by_key(|s| {
        (
            std::cmp::Reverse(s.video.published.unwrap_or(s.first_seen)),
            s.video.id.clone(),
        )
    });
    let channels: Vec<topics::Channel> = subs
        .channels
        .iter()
        .map(|sub| topics::Channel {
            id: &sub.id,
            name: &sub.name,
            titles: latest
                .iter()
                .filter(|s| s.video.channel_id.as_deref() == Some(sub.id.as_str()))
                .map(|s| s.video.title.as_str())
                .collect(),
        })
        .collect();
    let groups = topics::groups(&channels);
    Ok((groups, subs.channels.into_iter().map(|c| c.id).collect()))
}

/// The topic feeds. While the setting is off this reads nothing and asks
/// nobody. On, the groups are worked out from what is on this machine; with
/// `refresh`, a group whose results are missing or a day old is searched for
/// again, one search per phrase, while the day's budget of searches lasts
/// (`topics::BUDGET`, whatever the groups are). A group whose search fails is
/// left as it was and named in `errors`; the others are still searched for.
/// Videos from channels that are followed are left out. `wanted` is asked
/// before each group and between searches: once it says no (the client is
/// gone, the route was left, or the setting was turned off), nothing more is
/// searched for or saved.
pub fn topics(
    rt: &Runtime,
    paths: &Paths,
    refresh: bool,
    progress: &dyn Fn(usize, usize),
    wanted: &dyn Fn() -> bool,
) -> Result<Topics> {
    if !rt.cfg.topic_feeds {
        return Ok(Topics {
            enabled: false,
            groups: Vec::new(),
            errors: Vec::new(),
        });
    }
    let (all, followed) = topic_groups(paths)?;
    let shown = |hidden: &topics::Hidden| -> Vec<topics::Group> {
        all.iter()
            .filter(|g| !hidden.hides(g))
            .take(topics::MAX_GROUPS)
            .cloned()
            .collect()
    };
    let groups = shown(&store::load(&paths.topics())?);
    let mut results: topics::Results = store::load_cache(&paths.topics_cache())?;
    let mut errors = Vec::new();
    if refresh {
        let at = now();
        let due: Vec<&topics::Group> = groups
            .iter()
            .filter(|group| {
                results
                    .groups
                    .get(&group.key())
                    .is_none_or(|c| at - c.fetched_at >= topics::STALE)
            })
            .collect();
        // How far it is, counted in searches, over all the groups that are due.
        let total: usize = due.iter().map(|g| g.searches.len()).sum();
        let mut before = 0;
        for group in due {
            let so_far = before;
            before += group.searches.len();
            if !wanted() {
                bail!(crate::ytdlp::CANCELLED);
            }
            // It may have been hidden since this began, and the day has a budget.
            let hidden: topics::Hidden = store::load(&paths.topics())?;
            if hidden.hides(group) || !results.allows(group.searches.len(), now()) {
                continue;
            }
            let reading = Reading {
                topics: group.searches.clone(),
                subs: crate::lens::read::Subs::Exclude,
                ..Reading::default()
            };
            let found = search_topics(
                rt,
                paths,
                &reading,
                topics::PER_SEARCH,
                &|done, _| progress(so_far + done, total),
                wanted,
            );
            if !wanted() {
                bail!(crate::ytdlp::CANCELLED);
            }
            // The searches were made, whatever came of them.
            let _lock = store::lock(paths)?;
            let mut kept: topics::Results = store::load_cache(&paths.topics_cache())?;
            kept.spent(group.searches.len(), now());
            match found {
                Ok(videos) => {
                    kept.groups.insert(
                        group.key(),
                        Cached {
                            fetched_at: at,
                            videos,
                        },
                    );
                }
                Err(e) => errors.push(format!("{}: {e:#}", group.label)),
            }
            // What there is no group for any more goes once it is a day old:
            // a group that comes back sooner has not been searched for twice.
            let now_at = now();
            kept.groups.retain(|key, found| {
                groups.iter().any(|g| g.key() == *key) || now_at - found.fetched_at < topics::STALE
            });
            store::save_compact(&paths.topics_cache(), &kept)?;
            results = kept;
        }
    }
    // Asked again: one may have been hidden while this ran.
    let groups = shown(&store::load(&paths.topics())?);
    Ok(Topics {
        enabled: true,
        groups: groups
            .into_iter()
            .map(|group| {
                let found = results.groups.get(&group.key());
                TopicFeed {
                    fetched_at: found.map(|c| c.fetched_at),
                    // What is followed now is left out, whenever it was found.
                    videos: found
                        .map(|c| {
                            c.videos
                                .iter()
                                .filter(|v| {
                                    v.video
                                        .channel_id
                                        .as_ref()
                                        .is_some_and(|id| !followed.contains(id))
                                })
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default(),
                    group,
                }
            })
            .collect(),
        errors,
    })
}

/// Hides the topic feed `id`, and with it what it was made of: its channels
/// and its phrases are not shown or searched for as a topic again, in
/// whatever group they turn up.
pub fn hide_topic(paths: &Paths, id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("{id:?} is not a topic feed");
    }
    let (groups, _) = topic_groups(paths)?;
    let _lock = store::lock(paths)?;
    let mut hidden: topics::Hidden = store::load(&paths.topics())?;
    match groups.iter().find(|g| g.id == id) {
        Some(group) if !hidden.hides(group) => hidden.hide(group),
        Some(_) => return Ok(()),
        // Not a group any more: its id is all there is to go by.
        None if !hidden.ids.iter().any(|h| h == id) => {
            hidden.ids.push(id.to_string());
            let over = hidden.ids.len().saturating_sub(200);
            hidden.ids.drain(..over);
        }
        None => return Ok(()),
    }
    store::save(&paths.topics(), &hidden)
}

/// Forgets what the topic feeds found (when they are turned off).
pub fn forget_topics(paths: &Paths) -> Result<()> {
    let _lock = store::lock(paths)?;
    match std::fs::remove_file(paths.topics_cache()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
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
    done.errors = fetch.errors;
    if fetch.skipped > 0 {
        done.errors.push(format!(
            "The route changed during the refresh; {} channel(s) were not fetched.",
            fetch.skipped
        ));
    }
    saver.finish()?;
    Ok(done)
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

pub fn load_subs(paths: &Paths) -> Result<Subs> {
    store::load(&paths.subscriptions())
}

/// Returns the subscription and whether it is new. `current` says whether
/// `rt`'s route is still the one in use: a lookup that came back from a route
/// that has been left isn't added.
pub fn add_sub(
    rt: &Runtime,
    paths: &Paths,
    input: &str,
    current: &dyn Fn() -> bool,
) -> Result<(Sub, bool)> {
    let r = ids::parse_channel(input)
        .with_context(|| format!("{input:?} isn't a channel (@handle, channel URL or UC… ID)"))?;
    let (id, name) = match r {
        // The ID is all a subscription needs, and the channel's own feed
        // says what it is called. That needs no yt-dlp, which YouTube may
        // refuse (a bot check on this address) when RSS still answers, and
        // spares the one request that is a lookup of this channel in
        // particular. If the feed doesn't answer either, the ID will do as a
        // name until the channel is renamed.
        ids::ChannelRef::Id(id) => match feed::channel_title(rt, &id, current) {
            Ok(name) => (id, name),
            Err(_) if !current() => bail!(crate::inflight::ROUTE_CHANGED),
            Err(e) => {
                note!("qveil: couldn't read the name of {id} from its feed ({e:#}); using the ID");
                let name = id.clone();
                (id, name)
            }
        },
        // A handle has to be looked up, which only yt-dlp does.
        ids::ChannelRef::Url(url) => rt.yt.channel_info(&url)?,
    };
    // Only the file update is locked, not the lookup above.
    let _lock = store::lock(paths)?;
    if !current() {
        bail!(crate::inflight::ROUTE_CHANGED);
    }
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
    use crate::store::testing::{TempDir, listing_runs, listing_ytdlp, socks_spy};
    use std::fs;
    use std::sync::atomic::Ordering;

    /// A runtime whose every request fails at once (nothing listens on the
    /// proxy port) and whose yt-dlp is `ytdlp`.
    fn offline(paths: &Paths, ytdlp: &str) -> Runtime {
        let cfg = Config {
            ytdlp: ytdlp.into(),
            ..Config::default()
        };
        Runtime::new(cfg, paths, Some("http://127.0.0.1:1".into()), false).unwrap()
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

    fn rt_cfg(ytdlp: &str) -> Config {
        Config {
            ytdlp: ytdlp.into(),
            ..Config::default()
        }
    }

    /// A runtime on Direct whose channel feeds come from `rss_base`, a local
    /// stand-in for YouTube, and whose yt-dlp is `ytdlp`.
    fn with_feeds_at(paths: &Paths, rss_base: &str, ytdlp: &str) -> Runtime {
        let mut rt = Runtime::new(rt_cfg(ytdlp), paths, None, false).unwrap();
        rt.net = rt.net.allowing_http();
        rt.yt.net = rt.net.clone();
        rt.feed_base = rss_base.to_string();
        rt
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

    const SOMEONE: &str = "UCsBjURrPoezykLs9EqgamOA";

    #[test]
    fn subscribing_by_id_takes_the_name_from_the_feed_and_needs_no_yt_dlp() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let (base, hits) = crate::store::testing::rss_upstream(0);
        // A yt-dlp that would be counted if it were run: the bot check has
        // closed it to this address, in the case that matters.
        let rt = with_feeds_at(&paths, &base, &listing_ytdlp(&dir.0, ""));
        let (sub, added) = add_sub(&rt, &paths, SOMEONE, &|| true).unwrap();
        assert_eq!(
            (sub.name.as_str(), added),
            (&*format!("Name of {SOMEONE}"), true)
        );
        // A channel URL is the same thing.
        let other = format!("UC{:022}", 7);
        let (sub, _) = add_sub(
            &rt,
            &paths,
            &format!("https://www.youtube.com/channel/{other}/videos?si=x"),
            &|| true,
        )
        .unwrap();
        assert_eq!(sub.id, other);
        assert_eq!(hits.load(Ordering::SeqCst), 2, "one request each");
        assert_eq!(listing_runs(&dir.0), 0, "yt-dlp was not run");
        assert_eq!(load_subs(&paths).unwrap().channels.len(), 2);
        // Again: nothing new.
        assert!(!add_sub(&rt, &paths, SOMEONE, &|| true).unwrap().1);
    }

    #[test]
    fn subscribing_by_id_falls_back_to_the_id_when_the_feed_is_down() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let (base, hits) = crate::store::testing::rss_upstream(usize::MAX);
        let rt = with_feeds_at(&paths, &base, &listing_ytdlp(&dir.0, ""));
        let (sub, added) = add_sub(&rt, &paths, SOMEONE, &|| true).unwrap();
        assert_eq!((sub.name.as_str(), added), (SOMEONE, true));
        assert_eq!(hits.load(Ordering::SeqCst), 2, "tried twice, then gave up");
        assert_eq!(listing_runs(&dir.0), 0, "and did not ask yt-dlp either");
        // A first attempt that fails and a second that works is not a fallback.
        let (base, hits) = crate::store::testing::rss_upstream(1);
        let rt = with_feeds_at(&paths, &base, &listing_ytdlp(&dir.0, ""));
        let other = format!("UC{:022}", 3);
        let (sub, _) = add_sub(&rt, &paths, &other, &|| true).unwrap();
        assert_eq!(sub.name, format!("Name of {other}"));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_handle_still_needs_yt_dlp_and_the_feed_is_not_asked() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let (base, hits) = crate::store::testing::rss_upstream(0);
        let named = crate::store::testing::script(
            &dir.0.join("yt-dlp"),
            &format!(
                "echo x >> '{}'\necho '{{\"channel_id\":\"{SOMEONE}\",\"channel\":\"Someone\",\"entries\":[]}}'\n",
                dir.0.join("runs").display()
            ),
        );
        let rt = with_feeds_at(&paths, &base, &named);
        let (sub, added) = add_sub(&rt, &paths, "@someone", &|| true).unwrap();
        assert_eq!(
            (sub.id.as_str(), sub.name.as_str(), added),
            (SOMEONE, "Someone", true)
        );
        assert_eq!((listing_runs(&dir.0), hits.load(Ordering::SeqCst)), (1, 0));
    }

    #[test]
    fn a_name_that_came_back_from_a_route_that_was_left_is_not_added() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let (base, hits) = crate::store::testing::rss_upstream(0);
        let rt = with_feeds_at(&paths, &base, &listing_ytdlp(&dir.0, ""));
        let err = add_sub(&rt, &paths, SOMEONE, &|| false).unwrap_err();
        assert!(err.to_string().contains("route changed"), "{err}");
        // Not even asked: nothing goes out on the route that was left.
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert!(load_subs(&paths).unwrap().channels.is_empty());
        // The route changing while the feed was being read is the same: the
        // first look says all is well, and the feed fails, and the next says no.
        let (base, _) = crate::store::testing::rss_upstream(usize::MAX);
        let rt = with_feeds_at(&paths, &base, &listing_ytdlp(&dir.0, ""));
        let looks = std::sync::atomic::AtomicUsize::new(0);
        let err = add_sub(&rt, &paths, SOMEONE, &|| {
            looks.fetch_add(1, Ordering::SeqCst) == 0
        })
        .unwrap_err();
        assert!(err.to_string().contains("route changed"), "{err}");
    }
}
