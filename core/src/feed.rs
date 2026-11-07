//! Subscription feed without an account: one public RSS request per channel,
//! fetched in random order. If RSS is down (YouTube's feed endpoint
//! intermittently returns 404/500), fall back to the channel's Videos tab via
//! yt-dlp and date new uploads by when qveil first saw them.

use crate::app::Runtime;
use crate::ids::is_video_id;
use crate::subs::Sub;
use crate::video::Video;
use anyhow::{Context, Result, bail};
use rand::seq::SliceRandom;
use roxmltree::Node;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const RETENTION_SECS: i64 = 86_400 * 180;
const FALLBACK_LIMIT: usize = 15;
/// A refresh leaves out channels fetched this recently (unless forced), so
/// pressing refresh again doesn't repeat a burst of requests for exactly the
/// same channels, which is slow over Tor and adds to what can be correlated.
pub const FRESH_SECS: i64 = 15 * 60;

#[derive(Default, Serialize, Deserialize)]
pub struct FeedCache {
    pub seen: HashMap<String, Seen>,
    /// When each channel was last fetched successfully. A channel that failed
    /// has no new entry, so the next refresh tries it again.
    #[serde(default)]
    pub fetched_at: HashMap<String, i64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Seen {
    pub video: Video,
    /// When qveil first saw it; 0 = it was already there when the channel was first fetched.
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Serialize)]
pub struct Entry {
    #[serde(flatten)]
    pub video: Video,
    pub date: i64,
    /// `date` is when qveil first saw the video, not the real upload time.
    pub estimated: bool,
}

#[derive(Debug, PartialEq)]
pub enum Source {
    Rss,
    Fallback,
}

pub struct Fetched {
    pub channel_id: String,
    pub videos: Vec<Video>,
    pub source: Source,
}

/// How a refresh went, apart from the videos themselves, which were handed
/// to the sink one channel at a time (see `fetch_all`).
#[derive(Default)]
pub struct Fetch {
    /// Channels fetched and kept.
    pub fetched: usize,
    /// Of those, the ones whose RSS feed was down, so their dates are approximate.
    pub fallbacks: usize,
    pub errors: Vec<String>,
    /// Channels not fetched because the route changed meanwhile.
    pub skipped: usize,
}

/// What became of one channel's fetch.
enum Outcome {
    Kept {
        fallback: bool,
    },
    /// The route changed meanwhile, so there is nothing to keep.
    Late,
    Failed(anyhow::Error),
    /// Fetched, but `keep` couldn't store it.
    Unsaved(anyhow::Error),
}

/// After this many channels in a row that RSS failed for, the rest of the
/// refresh doesn't ask RSS again. YouTube's feed endpoint is often down for
/// everyone, and trying it twice per channel before each fallback is two
/// more requests for exactly your channels, each of which may wait out its
/// timeout (and, over Tor, a circuit of its own).
const RSS_GIVE_UP: usize = 3;

/// How RSS is doing during one refresh.
#[derive(Default)]
struct RssHealth {
    failed_in_a_row: AtomicUsize,
}

impl RssHealth {
    fn usable(&self) -> bool {
        self.failed_in_a_row.load(Ordering::Relaxed) < RSS_GIVE_UP
    }

    fn worked(&self) {
        self.failed_in_a_row.store(0, Ordering::Relaxed);
    }

    fn failed(&self) {
        self.failed_in_a_row.fetch_add(1, Ordering::Relaxed);
    }
}

/// Fetches the channels' feeds on `rt`'s route, handing each channel's
/// result to `keep` as soon as it is in, so that a refresh that is cut off
/// (the engine is killed, say) has kept what it got and the next one
/// doesn't start over. `current` says whether the route is still the one in
/// use: once it isn't, no further channel is started, because every request
/// would go out over the route the user just left, and what is still on its
/// way is dropped instead of kept. A `keep` that fails (the disk is full)
/// stops the refresh too, since everything after it would be lost.
pub fn fetch_all(
    rt: &Runtime,
    subs: &[Sub],
    workers: usize,
    current: &(dyn Fn() -> bool + Sync),
    keep: &(dyn Fn(Fetched) -> Result<()> + Sync),
) -> Fetch {
    let mut order: Vec<&Sub> = subs.iter().collect();
    order.shuffle(&mut rand::rng());
    let queue = Mutex::new(order);
    let results = Mutex::new(Fetch::default());
    let rss = RssHealth::default();
    let broken = AtomicBool::new(false);
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, 16).min(subs.len().max(1)) {
            s.spawn(|| {
                // Checked before each channel, so a channel that has been
                // started is not abandoned half-way.
                while current() && !broken.load(Ordering::Relaxed) {
                    let Some(sub) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    // Not under the results lock: saving waits on the store.
                    let outcome = match fetch_one(rt, sub, current, &rss) {
                        // It arrived after the route changed: dropped, not kept.
                        Ok(_) if !current() => Outcome::Late,
                        Ok(f) => {
                            let fallback = f.source == Source::Fallback;
                            match keep(f) {
                                Ok(()) => Outcome::Kept { fallback },
                                Err(e) => {
                                    broken.store(true, Ordering::Relaxed);
                                    Outcome::Unsaved(e)
                                }
                            }
                        }
                        // A failure after the route changed is probably that
                        // change cutting it off, and its result is moot.
                        Err(_) if !current() => Outcome::Late,
                        Err(e) => Outcome::Failed(e),
                    };
                    let mut res = results.lock().unwrap();
                    match outcome {
                        Outcome::Kept { fallback } => {
                            res.fetched += 1;
                            res.fallbacks += usize::from(fallback);
                        }
                        Outcome::Late => res.skipped += 1,
                        Outcome::Failed(e) => res.errors.push(format!("{}: {e:#}", sub.name)),
                        Outcome::Unsaved(e) => res.errors.push(format!(
                            "{}: couldn't save what was fetched: {e:#}",
                            sub.name
                        )),
                    }
                }
            });
        }
    });
    let mut res = results.into_inner().unwrap();
    res.skipped += queue.into_inner().unwrap().len();
    res
}

fn fetch_one(
    rt: &Runtime,
    sub: &Sub,
    current: &dyn Fn() -> bool,
    rss: &RssHealth,
) -> Result<Fetched> {
    let mut rss_err = None;
    if rss.usable() {
        for _ in 0..2 {
            // Each attempt is a request of its own, and the fallback below is a
            // slow one, so the route is looked at again before every step.
            if !current() {
                bail!("the route changed");
            }
            match fetch_rss(rt, &sub.id) {
                // It came over a route that is no longer in use.
                Ok(_) if !current() => bail!("the route changed"),
                Ok(videos) => {
                    rss.worked();
                    return Ok(fetched(sub, videos, Source::Rss));
                }
                Err(e) => rss_err = Some(e),
            }
        }
        rss.failed();
    }
    if !current() {
        bail!("the route changed");
    }
    let videos = rt
        .yt
        .channel_videos(&sub.id, FALLBACK_LIMIT)
        .with_context(|| match &rss_err {
            Some(e) => format!("RSS failed ({e:#}), fallback failed too"),
            None => "RSS was left out (it kept failing), and the fallback failed".to_string(),
        })?;
    Ok(fetched(sub, videos, Source::Fallback))
}

fn fetched(sub: &Sub, mut videos: Vec<Video>, source: Source) -> Fetched {
    for v in &mut videos {
        v.channel_id = Some(sub.id.clone());
        v.channel.get_or_insert_with(|| sub.name.clone());
    }
    Fetched {
        channel_id: sub.id.clone(),
        videos,
        source,
    }
}

fn fetch_rss(rt: &Runtime, channel_id: &str) -> Result<Vec<Video>> {
    let url = format!("{}/feeds/videos.xml?channel_id={channel_id}", rt.feed_base);
    let body = rt
        .web_client()?
        .get(url)
        .send()?
        .error_for_status()?
        .text()?;
    parse_rss(&body)
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.tag_name().name() == name)
}

fn child_text(n: Node, name: &str) -> Option<String> {
    child(n, name)
        .and_then(|c| c.text())
        .map(|t| t.trim().to_string())
}

pub fn parse_rss(xml: &str) -> Result<Vec<Video>> {
    let doc = roxmltree::Document::parse(xml).context("feed is not valid XML")?;
    let root = doc.root_element();
    if root.tag_name().name() != "feed" {
        bail!("not an Atom feed");
    }
    let mut out = Vec::new();
    for e in root.children().filter(|n| n.tag_name().name() == "entry") {
        let Some(id) = child_text(e, "videoId").filter(|i| is_video_id(i)) else {
            continue;
        };
        let link = child(e, "link")
            .and_then(|l| l.attribute("href"))
            .unwrap_or("");
        out.push(Video {
            title: child_text(e, "title").unwrap_or_else(|| id.clone()),
            channel: child(e, "author").and_then(|a| child_text(a, "name")),
            channel_id: child_text(e, "channelId"),
            published: child_text(e, "published")
                .and_then(|p| p.parse::<jiff::Timestamp>().ok())
                .map(|t| t.as_second()),
            duration: None,
            views: e
                .descendants()
                .find(|n| n.tag_name().name() == "statistics")
                .and_then(|s| s.attribute("views"))
                .and_then(|v| v.parse().ok()),
            short: link.contains("/shorts/"),
            id,
        });
    }
    Ok(out)
}

impl FeedCache {
    /// Whether the channel is due for a fetch: never fetched, or not within
    /// `FRESH_SECS`. A time in the future (the clock was set back) counts as stale.
    pub fn is_stale(&self, channel_id: &str, now: i64) -> bool {
        self.fetched_at
            .get(channel_id)
            .is_none_or(|&at| at > now || now - at >= FRESH_SECS)
    }

    pub fn merge(&mut self, fetched: &[Fetched], now: i64) {
        for f in fetched {
            self.fetched_at.insert(f.channel_id.clone(), now);
            let known = self
                .seen
                .values()
                .any(|s| s.video.channel_id.as_deref() == Some(f.channel_id.as_str()));
            for (pos, v) in f.videos.iter().enumerate() {
                let mut v = v.clone();
                // Fallback dates are approximations: keep them out of `published`
                // so a later RSS fetch can supply the real one.
                let approx = match f.source {
                    Source::Fallback => v.published.take(),
                    Source::Rss => None,
                };
                // Without any date, keep the listing order for videos found on
                // the same fetch, and treat a new channel's back catalogue as old.
                let first_seen = approx.unwrap_or(if known { now - pos as i64 } else { 0 });
                self.seen
                    .entry(v.id.clone())
                    .and_modify(|s| {
                        if s.first_seen == 0 {
                            s.first_seen = approx.unwrap_or(0);
                        }
                        let published = v.published.or(s.video.published);
                        let views = v.views.or(s.video.views);
                        let duration = v.duration.or(s.video.duration);
                        s.video = Video {
                            published,
                            views,
                            duration,
                            ..v.clone()
                        };
                        s.last_seen = now;
                    })
                    .or_insert(Seen {
                        video: v.clone(),
                        first_seen,
                        last_seen: now,
                    });
            }
        }
        self.seen.retain(|_, s| now - s.last_seen < RETENTION_SECS);
    }

    pub fn entries(&self, subs: &[Sub], no_shorts: bool, limit: usize) -> Vec<Entry> {
        let ids: HashSet<&str> = subs.iter().map(|s| s.id.as_str()).collect();
        let mut out: Vec<Entry> = self
            .seen
            .values()
            .filter(|s| {
                s.video
                    .channel_id
                    .as_deref()
                    .is_some_and(|c| ids.contains(c))
            })
            .filter(|s| !(no_shorts && s.video.short))
            .map(|s| Entry {
                video: s.video.clone(),
                date: s.video.published.unwrap_or(s.first_seen),
                estimated: s.video.published.is_none(),
            })
            .collect();
        out.sort_by(|a, b| {
            b.date
                .cmp(&a.date)
                .then_with(|| a.video.id.cmp(&b.video.id))
        });
        out.truncate(limit);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns:media="http://search.yahoo.com/mrss/" xmlns="http://www.w3.org/2005/Atom">
 <title>Fireship</title>
 <entry>
  <id>yt:video:5C_HPTJg5ek</id>
  <yt:videoId>5C_HPTJg5ek</yt:videoId>
  <yt:channelId>UCsBjURrPoezykLs9EqgamOA</yt:channelId>
  <title>Rust in 100 Seconds</title>
  <link rel="alternate" href="https://www.youtube.com/watch?v=5C_HPTJg5ek"/>
  <author><name>Fireship</name></author>
  <published>2021-09-08T15:00:12+00:00</published>
  <media:group>
   <media:title>Rust in 100 Seconds</media:title>
   <media:community><media:statistics views="2648797"/></media:community>
  </media:group>
 </entry>
 <entry>
  <yt:videoId>abcdefghijk</yt:videoId>
  <yt:channelId>UCsBjURrPoezykLs9EqgamOA</yt:channelId>
  <title>a short</title>
  <link rel="alternate" href="https://www.youtube.com/shorts/abcdefghijk"/>
  <published>2021-09-09T00:00:00+00:00</published>
 </entry>
</feed>"#;

    fn sub() -> Sub {
        Sub {
            id: "UCsBjURrPoezykLs9EqgamOA".into(),
            name: "Fireship".into(),
        }
    }

    #[test]
    fn parses_youtube_atom() {
        let vs = parse_rss(RSS).unwrap();
        assert_eq!(vs.len(), 2);
        assert_eq!(vs[0].title, "Rust in 100 Seconds");
        assert_eq!(vs[0].channel.as_deref(), Some("Fireship"));
        assert_eq!(vs[0].published, Some(1631113212));
        assert_eq!(vs[0].views, Some(2648797));
        assert!(!vs[0].short && vs[1].short);
        assert!(parse_rss("<html/>").is_err());
    }

    #[test]
    fn fallback_baseline_is_undated_then_new_uploads_are_dated() {
        let s = sub();
        let mk = |id: &str| Video {
            id: id.into(),
            title: id.into(),
            channel: None,
            channel_id: None,
            published: None,
            duration: None,
            views: None,
            short: false,
        };
        let mut cache = FeedCache::default();
        cache.merge(
            &[fetched(
                &s,
                vec![mk("aaaaaaaaaaa"), mk("bbbbbbbbbbb")],
                Source::Fallback,
            )],
            1000,
        );
        assert!(
            cache.seen.values().all(|x| x.first_seen == 0),
            "existing back catalogue must not look new"
        );

        cache.merge(
            &[fetched(
                &s,
                vec![mk("ccccccccccc"), mk("aaaaaaaaaaa")],
                Source::Fallback,
            )],
            5000,
        );
        let es = cache.entries(std::slice::from_ref(&s), false, 10);
        assert_eq!(es[0].video.id, "ccccccccccc");
        assert_eq!((es[0].date, es[0].estimated), (5000, true));
        assert_eq!(es[0].video.channel.as_deref(), Some("Fireship"));
    }

    #[test]
    fn fallback_approximate_dates_are_estimates() {
        let s = sub();
        let mut v = parse_rss(RSS).unwrap().remove(0);
        v.published = Some(1_600_000_000);
        let mut cache = FeedCache::default();
        cache.merge(
            &[fetched(&s, vec![v.clone()], Source::Fallback)],
            2_000_000_000,
        );
        let es = cache.entries(std::slice::from_ref(&s), false, 10);
        assert_eq!((es[0].date, es[0].estimated), (1_600_000_000, true));
        assert_eq!(es[0].video.published, None);
        // A later RSS fetch replaces the estimate with the real date.
        cache.merge(
            &[fetched(&s, parse_rss(RSS).unwrap(), Source::Rss)],
            2_000_000_100,
        );
        let es = cache.entries(std::slice::from_ref(&s), false, 10);
        assert_eq!((es[1].date, es[1].estimated), (1631113212, false));
    }

    #[test]
    fn rss_dates_survive_a_later_fallback_and_shorts_filter() {
        let s = sub();
        let mut cache = FeedCache::default();
        cache.merge(
            &[fetched(&s, parse_rss(RSS).unwrap(), Source::Rss)],
            2_000_000_000,
        );
        let mut undated = parse_rss(RSS).unwrap();
        undated.iter_mut().for_each(|v| v.published = None);
        cache.merge(&[fetched(&s, undated, Source::Fallback)], 2_000_000_100);
        let es = cache.entries(std::slice::from_ref(&s), true, 10);
        assert_eq!(es.len(), 1);
        assert_eq!((es[0].date, es[0].estimated), (1631113212, false));
        assert!(
            cache.entries(&[], false, 10).is_empty(),
            "unsubscribed channels are hidden"
        );
    }

    #[test]
    fn prunes_videos_gone_from_the_channel() {
        let s = sub();
        let mut cache = FeedCache::default();
        cache.merge(&[fetched(&s, parse_rss(RSS).unwrap(), Source::Rss)], 0);
        cache.merge(&[], RETENTION_SECS + 1);
        assert!(cache.seen.is_empty());
    }

    #[test]
    fn rss_is_left_out_after_three_channels_in_a_row_that_it_failed_for() {
        let rss = RssHealth::default();
        assert!(rss.usable());
        rss.failed();
        rss.failed();
        assert!(rss.usable(), "two failures are not yet a pattern");
        // A success in between starts the count again.
        rss.worked();
        rss.failed();
        rss.failed();
        assert!(rss.usable());
        rss.failed();
        assert!(!rss.usable());
        // Once it is left out, a stray success doesn't matter: nothing asks it.
        assert!(!rss.usable());
    }
}
