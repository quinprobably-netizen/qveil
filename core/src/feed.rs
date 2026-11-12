//! Subscription feed without an account: one public RSS request per channel,
//! fetched in random order. If RSS is down (YouTube's feed endpoint
//! intermittently returns 404/500), fall back to the channel's Videos tab via
//! yt-dlp and date new uploads by when qveil first saw them.

use crate::app::Runtime;
use crate::ids::is_video_id;
use crate::subs::Sub;
use crate::video::{LiveState, Video};
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
    /// What the live check last found running or announced on each channel
    /// (see `checked`). A channel's next check replaces them as a whole, and
    /// a refresh with the check off drops them all.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub live: HashMap<String, Vec<Mark>>,
}

/// A running or announced stream that the live check found.
#[derive(Clone, Serialize, Deserialize)]
pub struct Mark {
    pub video: Video,
    /// When a check first found it, which dates it where the feed doesn't
    /// list it: the checks after it don't move it up the feed again.
    pub found: i64,
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
    /// Channels not fetched because what was fetched before them couldn't
    /// be saved.
    pub unsaved: usize,
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
///
/// `progress(done, total)` is told each time a channel is finished with, in
/// order and one call at a time, however it went.
pub fn fetch_all(
    rt: &Runtime,
    subs: &[Sub],
    workers: usize,
    current: &(dyn Fn() -> bool + Sync),
    keep: &(dyn Fn(Fetched) -> Result<()> + Sync),
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Fetch {
    let mut order: Vec<&Sub> = subs.iter().collect();
    order.shuffle(&mut rand::rng());
    let queue = Mutex::new(order);
    // The results, and how many channels they are for.
    let results = Mutex::new((Fetch::default(), 0usize));
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
                    let mut guard = results.lock().unwrap();
                    let (res, done) = &mut *guard;
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
                    // Under the lock, which is what puts the calls in order.
                    *done += 1;
                    progress(*done, subs.len());
                }
            });
        }
    });
    let (mut res, _) = results.into_inner().unwrap();
    // What was never started was left for the reason the workers stopped.
    let left = queue.into_inner().unwrap().len();
    if broken.into_inner() {
        res.unsaved += left;
    } else {
        res.skipped += left;
    }
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

/// The text of the channel's feed, over the current route.
fn get_rss(rt: &Runtime, channel_id: &str) -> Result<String> {
    let url = format!("{}/feeds/videos.xml?channel_id={channel_id}", rt.feed_base);
    Ok(rt
        .web_client()?
        .get(url)
        .send()?
        .error_for_status()?
        .text()?)
}

fn fetch_rss(rt: &Runtime, channel_id: &str) -> Result<Vec<Video>> {
    parse_rss(&get_rss(rt, channel_id)?)
}

/// A channel's name from its RSS feed, which needs no yt-dlp (and so works
/// from an address that YouTube has bot-checked, where yt-dlp doesn't). Two
/// attempts, as for a feed: this endpoint fails now and then. `current` says
/// whether the route is still the one in use.
pub fn channel_title(rt: &Runtime, channel_id: &str, current: &dyn Fn() -> bool) -> Result<String> {
    let mut last = None;
    for _ in 0..2 {
        if !current() {
            bail!(crate::inflight::ROUTE_CHANGED);
        }
        match get_rss(rt, channel_id).and_then(|xml| parse_feed_title(&xml)) {
            Ok(title) => return Ok(title),
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("two attempts were made"))
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.tag_name().name() == name)
}

fn child_text(n: Node, name: &str) -> Option<String> {
    child(n, name)
        .and_then(|c| c.text())
        .map(|t| t.trim().to_string())
}

/// Whether an entry says it is a running or upcoming stream. YouTube's feed
/// has no documented marker for that, so this reads the one element its own
/// APIs use (`liveBroadcastContent`), in case the feed carries it. Streams that
/// it doesn't mark come through as ordinary videos here, and are marked when
/// yt-dlp lists them (searches, the fallback).
fn announces_live(entry: Node) -> Option<LiveState> {
    entry
        .descendants()
        .filter(|n| n.tag_name().name() == "liveBroadcastContent")
        .find_map(|n| match n.text().map(str::trim) {
            Some("live") => Some(LiveState::Now),
            Some("upcoming") => Some(LiveState::Upcoming),
            _ => None,
        })
}

/// The channel's name: the title of its feed, which is how the channel itself
/// is called, without a lookup of the channel.
pub fn parse_feed_title(xml: &str) -> Result<String> {
    let doc = roxmltree::Document::parse(xml).context("feed is not valid XML")?;
    let root = doc.root_element();
    if root.tag_name().name() != "feed" {
        bail!("not an Atom feed");
    }
    // The title is what is shown in a list, so it is one short line of text.
    let title: String = child_text(root, "title")
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    let title = title.trim();
    if title.is_empty() {
        bail!("the feed has no title");
    }
    Ok(title.to_string())
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
            live: announces_live(e),
            starts: None,
            watching: None,
            streamed: false,
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
                        // A finished stream stays one: the feed doesn't say.
                        let streamed = v.streamed || s.video.streamed;
                        s.video = Video {
                            published,
                            views,
                            duration,
                            streamed,
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

    /// Takes what the live check found on a channel's Streams tab. Its
    /// running and announced streams replace what the last check found
    /// there. Its past broadcasts join the feed, each once: the RSS feed
    /// lists most of them already, and those are only marked as streamed
    /// (and given what the feed lacks, such as their length). The Streams
    /// tab's dates are approximate, so they are kept as estimates, as a
    /// fallback's are. Anything else it lists is left alone. This is not a
    /// fetch of the channel's feed, which `fetched_at` stays about.
    pub fn checked(&mut self, channel_id: &str, found: &[Video], now: i64) {
        let before = self.live.remove(channel_id).unwrap_or_default();
        let marks: Vec<Mark> = found
            .iter()
            .filter(|v| v.live.is_some())
            .map(|v| Mark {
                video: v.clone(),
                found: before
                    .iter()
                    .find(|m| m.video.id == v.id)
                    .map_or(now, |m| m.found),
            })
            .collect();
        if !marks.is_empty() {
            self.live.insert(channel_id.to_string(), marks);
        }
        for v in found.iter().filter(|v| v.streamed && v.live.is_none()) {
            let mut v = v.clone();
            let approx = v.published.take();
            self.seen
                .entry(v.id.clone())
                .and_modify(|s| {
                    s.video.streamed = true;
                    s.video.duration = s.video.duration.or(v.duration);
                    s.video.views = s.video.views.or(v.views);
                    if s.first_seen == 0 {
                        s.first_seen = approx.unwrap_or(0);
                    }
                    s.last_seen = now;
                })
                .or_insert(Seen {
                    video: v.clone(),
                    first_seen: approx.unwrap_or(0),
                    last_seen: now,
                });
        }
    }

    /// Drops every mark the live check set. Whether there were any.
    pub fn forget_live(&mut self) -> bool {
        let had = !self.live.is_empty();
        self.live.clear();
        had
    }

    pub fn entries(&self, subs: &[Sub], no_shorts: bool, limit: usize) -> Vec<Entry> {
        let ids: HashSet<&str> = subs.iter().map(|s| s.id.as_str()).collect();
        let followed = |v: &Video| v.channel_id.as_deref().is_some_and(|c| ids.contains(c));
        // What the live check found, by video: it is fresher than the feed
        // about whether a stream is running.
        let marks: HashMap<&str, &Mark> = self
            .live
            .iter()
            .filter(|(channel, _)| ids.contains(channel.as_str()))
            .flat_map(|(_, marks)| marks)
            .map(|m| (m.video.id.as_str(), m))
            .collect();
        let seen = self.seen.values().filter(|s| followed(&s.video)).map(|s| {
            let mut video = s.video.clone();
            if let Some(m) = marks.get(video.id.as_str()) {
                video.live = m.video.live;
                video.starts = m.video.starts;
                video.watching = m.video.watching.or(video.watching);
            }
            Entry {
                date: video.published.unwrap_or(s.first_seen),
                estimated: video.published.is_none(),
                video,
            }
        });
        // The streams the feed doesn't list (yet), by when they were found.
        let unlisted = marks
            .values()
            .filter(|m| !self.seen.contains_key(&m.video.id))
            .map(|m| Entry {
                video: Video {
                    published: None,
                    ..m.video.clone()
                },
                date: m.found,
                estimated: true,
            });
        let mut out: Vec<Entry> = seen
            .chain(unlisted)
            .filter(|e| !(no_shorts && e.video.short))
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
    fn the_channels_name_is_the_title_of_its_feed() {
        assert_eq!(parse_feed_title(RSS).unwrap(), "Fireship");
        // One short line, whatever the feed says.
        let long = format!(
            "<feed xmlns=\"http://www.w3.org/2005/Atom\"><title>  A\tB\nC{}  </title></feed>",
            "x".repeat(500)
        );
        let title = parse_feed_title(&long).unwrap();
        assert!(
            title.starts_with("A B C") && title.chars().count() <= 200,
            "{title}"
        );
        for bad in [
            "<feed xmlns=\"http://www.w3.org/2005/Atom\"/>",
            "<feed xmlns=\"http://www.w3.org/2005/Atom\"><title>  </title></feed>",
            "<html/>",
            "not xml",
        ] {
            assert!(parse_feed_title(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_stream_that_the_feed_marks_as_live_or_upcoming_says_which() {
        let xml = |kind: &str| {
            format!(
                r#"<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns="http://www.w3.org/2005/Atom"><entry>
                <yt:videoId>5C_HPTJg5ek</yt:videoId><title>t</title>
                <yt:liveBroadcastContent>{kind}</yt:liveBroadcastContent></entry></feed>"#
            )
        };
        for (kind, live) in [
            ("live", Some(LiveState::Now)),
            ("upcoming", Some(LiveState::Upcoming)),
            ("none", None),
            ("", None),
        ] {
            assert_eq!(parse_rss(&xml(kind)).unwrap()[0].live, live, "{kind:?}");
        }
        // The feed as it is today carries nothing of the kind.
        assert!(parse_rss(RSS).unwrap().iter().all(|v| v.live.is_none()));
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
            live: None,
            starts: None,
            watching: None,
            streamed: false,
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

    fn checked_as(id: &str, status: &str) -> Video {
        let v: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"entries":[{{"id":"{id}","title":"from the Streams tab","live_status":"{status}","duration":60.0,"timestamp":1500000000}}]}}"#
        ))
        .unwrap();
        let mut v = crate::ytdlp::entries(&v).remove(0);
        v.channel_id = Some(sub().id);
        v
    }

    #[test]
    fn a_finished_stream_stays_one_after_the_next_rss_fetch() {
        let s = sub();
        let mut cache = FeedCache::default();
        let rss = || fetched(&s, parse_rss(RSS).unwrap(), Source::Rss);
        cache.merge(&[rss()], 2_000_000_000);
        cache.checked(
            &s.id,
            &[checked_as("5C_HPTJg5ek", "was_live")],
            2_000_000_050,
        );
        cache.merge(&[rss()], 2_000_000_100);
        let es = cache.entries(std::slice::from_ref(&s), false, 10);
        assert_eq!(es.len(), 2, "not added a second time");
        let e = &es[1];
        assert_eq!(e.video.id, "5C_HPTJg5ek");
        assert!(e.video.streamed);
        // What the feed says comes first; the Streams tab fills in what it lacks.
        assert_eq!((e.date, e.estimated), (1631113212, false));
        assert_eq!(e.video.title, "Rust in 100 Seconds");
        assert_eq!(e.video.duration, Some(60));
    }

    #[test]
    fn a_running_stream_the_feed_lists_is_shown_once_and_live() {
        let s = sub();
        let mut cache = FeedCache::default();
        cache.merge(
            &[fetched(&s, parse_rss(RSS).unwrap(), Source::Rss)],
            2_000_000_000,
        );
        cache.checked(
            &s.id,
            &[checked_as("5C_HPTJg5ek", "is_live")],
            2_000_000_050,
        );
        let es = cache.entries(std::slice::from_ref(&s), false, 10);
        assert_eq!(es.len(), 2);
        assert_eq!(es[1].video.live, Some(LiveState::Now));
        assert_eq!((es[1].date, es[1].estimated), (1631113212, false));
        // A stream the feed doesn't list is dated by when a check first found
        // it, which the checks after it keep.
        let upcoming = checked_as("ccccccccccc", "is_upcoming");
        cache.checked(&s.id, std::slice::from_ref(&upcoming), 2_000_000_100);
        cache.checked(&s.id, &[upcoming], 2_000_000_200);
        let es = cache.entries(std::slice::from_ref(&s), false, 10);
        assert_eq!(es.len(), 3);
        assert_eq!(es[0].video.id, "ccccccccccc");
        assert_eq!(es[0].video.live, Some(LiveState::Upcoming));
        assert_eq!((es[0].date, es[0].estimated), (2_000_000_100, true));
        // The earlier one was replaced by that check, and is no longer live.
        assert_eq!(es[2].video.live, None);
        // Marks are kept for channels that are followed, and only shown for them.
        assert!(cache.entries(&[], false, 10).is_empty());
        assert!(cache.forget_live());
        assert!(!cache.forget_live(), "nothing left to forget");
        assert_eq!(cache.entries(std::slice::from_ref(&s), false, 10).len(), 2);
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
