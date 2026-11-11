//! Topic feeds: groups of the channels you follow, searched for by
//! themselves within a day's budget of searches.

use super::lens::{search_topics, searches_needed};
use super::now;
use super::runtime::{Pacing, Runtime};
use super::subs::load_subs;
use crate::feed::{self, FeedCache};
use crate::lens::Cached;
use crate::lens::rank::Ranked;
use crate::lens::read::Reading;
use crate::store::{self, Paths};
use crate::topics;
use crate::util::lock;
use anyhow::{Context, Result, bail};
use rand::seq::SliceRandom;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Mutex;

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

/// One topic feed refresh at a time in this process: a second one waits for
/// the first, then finds fresh what that one found, so that no group is
/// searched for twice.
static TOPICS_REFRESH: Mutex<()> = Mutex::new(());

/// What a refresh says when the day's budget has no room for a group's
/// searches: all of it used, or only some, by this process or another.
pub(crate) const TOPICS_SPENT: &str = "The last day's topic searches leave no room for what is due (a search that failed counts too); it can be searched for once enough of them are a day old.";

/// What a refresh says when the day's searches can't be counted.
fn topics_uncounted(e: &anyhow::Error) -> String {
    format!(
        "No more topic searches are made while the count of the last day's searches can't be read ({e:#})."
    )
}

/// The topic searches of the last day, `settle`d, under the store lock. A
/// file that can't be read is an error, not an empty day: it could only make
/// for more searches than the budget. An older version kept the times in the
/// results cache; they are taken from there the first time.
fn load_topic_searches(paths: &Paths, now: i64) -> Result<topics::Searched> {
    let path = paths.topic_searches();
    let mut searched: topics::Searched = match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let old: topics::Results = store::load_cache(&paths.topics_cache())?;
            topics::Searched { at: old.searched }
        }
        _ => store::load(&path)?,
    };
    let changed = searched.settle(now);
    if searched.at.is_empty() {
        // Nothing left of the day: nothing is kept, not even an empty list.
        remove_if_there(&path)?;
    } else if changed || !path.exists() {
        // Once moved over, or once a time is put right or forgotten, it is
        // written down.
        store::save_compact(&path, &searched)?;
    }
    Ok(searched)
}

fn remove_if_there(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("deleting {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// What the engine does with the topic feeds' files as it starts: the times
/// of searches that are a day old are deleted, and while the setting is off
/// so is what was found (left behind by a `forget_topics` that failed, or by
/// a setting changed by hand). With neither file there it touches nothing.
pub fn tidy_topics(paths: &Paths, on: bool) -> Result<()> {
    let there = |p: std::path::PathBuf| std::fs::symlink_metadata(p).is_ok();
    if !there(paths.topic_searches()) && !there(paths.topics_cache()) {
        return Ok(());
    }
    if !on {
        return forget_topics(paths);
    }
    let _lock = store::lock(paths)?;
    load_topic_searches(paths, now()).map(|_| ())
}

/// How many more topic searches the day has room for.
fn topic_searches_left(paths: &Paths) -> Result<usize> {
    let _lock = store::lock(paths)?;
    Ok(load_topic_searches(paths, now())?.left())
}

/// Notes one topic search, before it is sent. False, and nothing is noted,
/// when the day's are used up.
fn charge_topic_search(paths: &Paths) -> Result<bool> {
    let _lock = store::lock(paths)?;
    let at = now();
    let mut searched = load_topic_searches(paths, at)?;
    if !searched.take(at) {
        return Ok(false);
    }
    store::save_compact(&paths.topic_searches(), &searched)?;
    Ok(true)
}

/// The topic feeds. While the setting is off this reads nothing and asks
/// nobody. On, the groups are worked out from what is on this machine; with
/// `refresh`, a group whose results are missing or a day old is searched for
/// again, one search per phrase, while the day's budget of searches lasts
/// (`topics::BUDGET`, whatever the groups are). Each search is counted before
/// it is sent, so one that is cut off or fails counts too; a group the budget
/// has no room for is not begun. A group whose search fails is left as it was
/// and named in `errors`, and the refresh stops there (offline, every group's
/// first search would fail, and count). Videos from
/// channels that are followed are left out. `wanted` is asked before each
/// group, between searches and once more before saving, under the lock: once
/// it says no (the client is gone, the route was left, or the setting was
/// turned off), nothing more is searched for or saved.
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
        let _one_at_a_time = lock(&TOPICS_REFRESH);
        if !wanted() {
            bail!(crate::ytdlp::CANCELLED);
        }
        // What a refresh that this one waited for found.
        results = store::load_cache(&paths.topics_cache())?;
        let at = now();
        let due = due_groups(&groups, &results, at);
        // One refresh's searches are spaced out across its groups too.
        let pacing = Pacing::of(rt);
        // How far it is, counted in searches, over all the groups that are due.
        let total: usize = due.iter().map(|g| g.searches.len()).sum();
        let mut before = 0;
        // Why groups were not searched for, said once however many there are.
        let mut held_back: Option<String> = None;
        for group in due {
            let so_far = before;
            before += group.searches.len();
            if !wanted() {
                bail!(crate::ytdlp::CANCELLED);
            }
            let found = match search_group(
                rt,
                paths,
                group,
                &|done, _| progress(so_far + done, total),
                wanted,
                &pacing,
            )? {
                GroupSearch::Skipped => continue,
                GroupSearch::Uncounted(why) => {
                    held_back = Some(why);
                    break;
                }
                GroupSearch::NoRoom => {
                    held_back.get_or_insert_with(|| TOPICS_SPENT.into());
                    continue;
                }
                GroupSearch::Refused(why) => {
                    held_back = Some(why);
                    continue;
                }
                GroupSearch::Done(found) => found,
            };
            // A search that failed most likely failed for want of a
            // connection, which the next group's would too, and each would
            // count: the refresh stops here.
            if save_group(paths, group, &groups, found, at, wanted, &mut errors)? {
                break;
            }
        }
        errors.extend(held_back);
        results = store::load_cache(&paths.topics_cache())?;
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

/// The groups of `groups` whose results in `results` are missing or a day
/// old at `at`, in a new random order.
fn due_groups<'a>(
    groups: &'a [topics::Group],
    results: &topics::Results,
    at: i64,
) -> Vec<&'a topics::Group> {
    let mut due: Vec<&topics::Group> = groups
        .iter()
        .filter(|group| topics::due(results.groups.get(&group.key()), at))
        .collect();
    due.shuffle(&mut rand::rng());
    due
}

/// What became of one group that a refresh had due.
enum GroupSearch {
    /// Searched for: what was found, or why it wasn't.
    Done(Result<Vec<Ranked>>),
    /// Hidden since the refresh began, or found by another process.
    Skipped,
    /// The day's budget has no room for all of its searches, so it was not begun.
    NoRoom,
    /// Cut short by the budget (another process spent it): why, in words.
    Refused(String),
    /// The day's searches can't be counted, so no more are made: why.
    Uncounted(String),
}

/// Searches for one group that is due, each search counted before it is
/// sent. An error (`CANCELLED`) once the answer is no longer wanted.
fn search_group(
    rt: &Runtime,
    paths: &Paths,
    group: &topics::Group,
    progress: &dyn Fn(usize, usize),
    wanted: &dyn Fn() -> bool,
    pacing: &Pacing,
) -> Result<GroupSearch> {
    // Since this began it may have been hidden, or found by another
    // process; and the day has a budget, which a group must fit in.
    {
        let _lock = store::lock(paths)?;
        let hidden: topics::Hidden = store::load(&paths.topics())?;
        let kept: topics::Results = store::load_cache(&paths.topics_cache())?;
        if hidden.hides(group) || !topics::due(kept.groups.get(&group.key()), now()) {
            return Ok(GroupSearch::Skipped);
        }
    }
    let reading = Reading {
        topics: group.searches.clone(),
        subs: crate::lens::read::Subs::Exclude,
        ..Reading::default()
    };
    // Room for every search the group may send, so that the budget
    // doesn't cut it off half-way.
    match topic_searches_left(paths) {
        Err(e) => return Ok(GroupSearch::Uncounted(topics_uncounted(&e))),
        Ok(left) if left < searches_needed(&reading, now()) => return Ok(GroupSearch::NoRoom),
        Ok(_) => {}
    }
    // Each search is counted before it is sent, so what is sent is
    // counted, however this ends.
    let refused: std::cell::RefCell<Option<String>> = Default::default();
    let charge = || -> Result<()> {
        let why = match charge_topic_search(paths) {
            Ok(true) => return Ok(()),
            Ok(false) => TOPICS_SPENT.to_string(),
            Err(e) => topics_uncounted(&e),
        };
        *refused.borrow_mut() = Some(why.clone());
        bail!(why)
    };
    let found = search_topics(
        rt,
        paths,
        &reading,
        topics::PER_SEARCH,
        progress,
        &charge,
        wanted,
        pacing,
    );
    if !wanted() {
        bail!(crate::ytdlp::CANCELLED);
    }
    // Cut short by the budget (another process spent it), so there is
    // nothing to keep.
    if let Some(why) = refused.take() {
        return Ok(GroupSearch::Refused(why));
    }
    Ok(GroupSearch::Done(found))
}

/// Keeps what the searches for `group` found (searched for at `at`), or
/// says in `errors` why they failed, under the store lock. Whether they
/// failed. What there is no group for any more in `groups` goes once it is
/// due again.
fn save_group(
    paths: &Paths,
    group: &topics::Group,
    groups: &[topics::Group],
    found: Result<Vec<Ranked>>,
    at: i64,
    wanted: &dyn Fn() -> bool,
    errors: &mut Vec<String>,
) -> Result<bool> {
    let _lock = store::lock(paths)?;
    // Asked again now that it is this one's turn: the setting may have
    // been turned off while it waited, and what was found deleted.
    if !wanted() {
        bail!(crate::ytdlp::CANCELLED);
    }
    let mut kept: topics::Results = store::load_cache(&paths.topics_cache())?;
    let failed = match found {
        Ok(videos) => {
            kept.groups.insert(
                group.key(),
                Cached {
                    fetched_at: at,
                    videos,
                },
            );
            false
        }
        Err(e) => {
            errors.push(format!("{}: {e:#}", group.label));
            true
        }
    };
    // What there is no group for any more goes once it is due again:
    // a group that comes back sooner has not been searched for twice.
    let now_at = now();
    kept.groups.retain(|key, found| {
        groups.iter().any(|g| g.key() == *key) || !topics::due(Some(found), now_at)
    });
    store::save_compact(&paths.topics_cache(), &kept)?;
    Ok(failed)
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

/// Forgets what the topic feeds found (when they are turned off). When the
/// day's searches were sent is kept (`Paths::topic_searches`), until each is
/// a day old: forgotten, turning them on again would start the count over.
/// Those that are a day old already go now. Call it once the setting is off
/// for everything that is running, so that a refresh that was about to save
/// asks `wanted` and finds it is not.
pub fn forget_topics(paths: &Paths) -> Result<()> {
    let _lock = store::lock(paths)?;
    // First, while an older version's times may still be in the results.
    let settled = load_topic_searches(paths, now());
    remove_if_there(&paths.topics_cache())?;
    settled.map(|_| ())
}

#[cfg(test)]
mod tests {
    use crate::app::lens::{search_topics, searches_needed};
    use crate::app::now;
    use crate::app::runtime::{Pacing, Runtime};
    use crate::app::testing::{asked, rt_cfg, unfiltered_ytdlp};
    use crate::lens;
    use crate::lens::read::read;
    use crate::store::testing::TempDir;

    #[test]
    fn a_topic_searched_again_without_its_filter_counts_twice_and_room_is_made_for_that() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let rt = Runtime::new(rt_cfg(&unfiltered_ytdlp(&dir.0)), &paths, None, false).unwrap();
        // YouTube has a filter for the length, which finds nothing here.
        let reading = read("bread, sourdough over 30 min", now());
        assert!(lens::sp::sp(&reading, now()).is_some());
        let charged = std::cell::Cell::new(0);
        let found = search_topics(
            &rt,
            &paths,
            &reading,
            5,
            &|_, _| {},
            &|| {
                charged.set(charged.get() + 1);
                Ok(())
            },
            &|| true,
            &Pacing::none(),
        )
        .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(asked(&dir).len(), 4, "{:?}", asked(&dir));
        assert_eq!(charged.get(), 4, "every search that was sent is counted");
        // So a refresh needs room for both of each topic's searches before it
        // begins, or the budget would cut it off half-way.
        assert_eq!(searches_needed(&reading, now()), 4);
        assert_eq!(searches_needed(&read("bread, sourdough", now()), now()), 2);
    }
}
