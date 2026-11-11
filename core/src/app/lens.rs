//! Searches: one typed in, a search with rules in it, and a lens's, whose
//! topics are searched for one by one and ranked here.

use super::history::load_history;
use super::now;
use super::runtime::{Pacing, Runtime};
use super::subs::load_subs;
use crate::lens::rank::{self, Found, Ranked};
use crate::lens::read::{Reading, Tag, read};
use crate::lens::{self, Cached, Lens};
use crate::store::Paths;
use crate::video::Video;
use anyhow::{Result, bail};
use rand::seq::SliceRandom;
use serde::Serialize;
use std::collections::HashSet;

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
        LensRefresh::Stale => lens::due(stored.as_ref(), at, lens::STALE),
        LensRefresh::Force => true,
    };
    let stored = if due {
        let videos = search_topics(
            rt,
            paths,
            &reading,
            lens::PER_TOPIC,
            progress,
            &|| Ok(()),
            wanted,
            &Pacing::of(rt),
        )?;
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
    search_topics(
        rt,
        paths,
        reading,
        limit,
        &|_, _| {},
        &|| Ok(()),
        wanted,
        &Pacing::none(),
    )
}

/// How many searches `search_topics` may send for `reading` at `now`: one
/// per topic, and where YouTube's own filter is used, a second one for a
/// topic that it finds nothing for.
pub(super) fn searches_needed(reading: &Reading, now: i64) -> usize {
    let per_topic = if lens::sp::sp(reading, now).is_some() {
        2
    } else {
        1
    };
    reading.topics.len() * per_topic
}

/// One search per topic, one after the other in random order (the ranking
/// doesn't depend on it), then the ranking. The searches are all that leaves
/// the machine: the rules and the order are applied here. `pacing` spaces
/// them out. `charge` is called before each search is sent, and an error
/// from it stops the searching there.
#[allow(clippy::too_many_arguments)]
pub(super) fn search_topics(
    rt: &Runtime,
    paths: &Paths,
    reading: &Reading,
    per_topic: usize,
    progress: &dyn Fn(usize, usize),
    charge: &dyn Fn() -> Result<()>,
    wanted: &dyn Fn() -> bool,
    pacing: &Pacing,
) -> Result<Vec<Ranked>> {
    reading.check()?;
    // What the ranking needs from this machine is read first: if it can't
    // be, the searches would be sent for nothing.
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
    let total = reading.topics.len();
    progress(0, total);
    // Where YouTube has a filter that contains a rule, it does the first
    // pass; the rules are applied here afterwards either way.
    let filter = lens::sp::sp(reading, now());
    let mut found = Vec::new();
    let mut order: Vec<usize> = (0..total).collect();
    order.shuffle(&mut rand::rng());
    for (done, topic) in order.into_iter().enumerate() {
        let query = &reading.topics[topic];
        let mut search = |sp: Option<&str>| {
            if !pacing.before_search(wanted) {
                bail!(crate::ytdlp::CANCELLED);
            }
            charge()?;
            // Counting it may have waited for the lock, while the answer
            // stopped being wanted: then it stays counted, and isn't sent.
            if !wanted() {
                bail!(crate::ytdlp::CANCELLED);
            }
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
        progress(done + 1, total);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::now;
    use crate::app::runtime::{Pacing, Runtime};
    use crate::app::testing::{asked, rt_cfg, unfiltered_ytdlp};
    use crate::lens;
    use crate::lens::read::read;
    use crate::store::Paths;
    use crate::store::testing::TempDir;
    use crate::util::lock;
    use std::collections::HashSet;
    use std::fs;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    /// A runtime whose pause between searches waits for nothing, and says
    /// each time whether the store was free to lock meanwhile.
    fn pausing(paths: &Paths, ytdlp: &str) -> (Runtime, Arc<Mutex<Vec<bool>>>) {
        let paused: Arc<Mutex<Vec<bool>>> = Arc::default();
        let mut rt = Runtime::new(rt_cfg(ytdlp), paths, None, false).unwrap();
        let (seen, lock_file) = (Arc::clone(&paused), paths.lock_file());
        rt.pause = Arc::new(move |_| {
            let free = fs::File::create(&lock_file).unwrap().try_lock().is_ok();
            lock(&seen).push(free);
        });
        (rt, paused)
    }

    #[test]
    fn a_lens_refresh_pauses_between_its_searches_with_nothing_locked() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let (rt, paused) = pausing(&paths, &unfiltered_ytdlp(&dir.0));
        let id = lens::add(&paths, "bread, sourdough, rye", now())
            .unwrap()
            .id;
        let found = lens(&rt, &paths, &id, LensRefresh::Force, &|_, _| {}, &|| true).unwrap();
        assert_eq!(found.videos.len(), 3);
        assert_eq!(asked(&dir).len(), 3);
        // Before the second and the third, not before the first.
        assert_eq!(*lock(&paused), [true, true]);
    }

    #[test]
    fn a_refresh_that_stops_being_wanted_while_it_pauses_sends_nothing_more() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let ytdlp = unfiltered_ytdlp(&dir.0);
        let mut rt = Runtime::new(rt_cfg(&ytdlp), &paths, None, false).unwrap();
        let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hang_up = Arc::clone(&gone);
        // The window goes away during the first pause.
        rt.pause = Arc::new(move |wanted| {
            hang_up.store(true, Ordering::SeqCst);
            assert!(!wanted());
        });
        let id = lens::add(&paths, "bread, sourdough, rye", now())
            .unwrap()
            .id;
        let wanted = || !gone.load(Ordering::SeqCst);
        let r = lens(&rt, &paths, &id, LensRefresh::Force, &|_, _| {}, &wanted);
        assert_eq!(r.err().unwrap().to_string(), crate::ytdlp::CANCELLED);
        assert_eq!(asked(&dir).len(), 1);
        assert!(!paths.lens_cache(&id).exists());
    }

    #[test]
    fn a_search_typed_in_does_not_pause() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let (rt, paused) = pausing(&paths, &unfiltered_ytdlp(&dir.0));
        let reading = crate::lens::read::read_search("bread, sourdough over 10 min", now());
        smart_search(&rt, &paths, &reading, 5, &|| true).unwrap();
        assert!(asked(&dir).len() >= 2);
        assert!(lock(&paused).is_empty());
    }

    #[test]
    fn the_topics_are_searched_in_a_new_order_each_time_and_ranked_the_same() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let rt = Runtime::new(rt_cfg(&unfiltered_ytdlp(&dir.0)), &paths, None, false).unwrap();
        let reading = read("bread, sourdough, rye", now());
        let mut orders = HashSet::new();
        for round in 0..20 {
            let found = search_topics(
                &rt,
                &paths,
                &reading,
                5,
                &|_, _| {},
                &|| Ok(()),
                &|| true,
                &Pacing::none(),
            )
            .unwrap();
            let why: Vec<&str> = found.iter().map(|r| r.why[0].as_str()).collect();
            assert_eq!(why, ["bread", "sourdough", "rye"]);
            orders.insert(asked(&dir)[round * 3..].to_vec());
        }
        assert!(orders.len() > 1, "always {orders:?}");
    }
}
