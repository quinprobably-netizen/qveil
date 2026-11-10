//! Putting what a lens's searches found in order.
//!
//! The order is worked out here, from what each search returned and what is
//! on this machine (the subscriptions, the history), by rules that can be
//! read below. Nothing about the user is sent anywhere to get it.

use super::read::Reading;
use crate::video::Video;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// How much being new is worth at most, and how fast that halves, in days.
const RECENCY: f64 = 0.5;
const HALF_LIFE: f64 = 30.0;
/// What a channel the user follows is worth.
const FOLLOWED: f64 = 0.25;
/// One channel in a row, and among the first results.
const IN_A_ROW: usize = 2;
const PER_CHANNEL: usize = 4;
const FIRST: usize = 40;

/// A video as one topic's search returned it.
#[derive(Clone, Debug)]
pub struct Found {
    pub video: Video,
    /// Which of the reading's topics found it.
    pub topic: usize,
    /// Its place in that search's results, from 0.
    pub rank: usize,
}

/// A video that was kept, and why: its topic, then each rule it met.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ranked {
    #[serde(flatten)]
    pub video: Video,
    pub why: Vec<String>,
}

/// What is known on this machine that the order depends on.
pub struct Context<'a> {
    /// The IDs of the channels the user follows.
    pub subscribed: &'a HashSet<String>,
    /// The IDs of videos not to show again.
    pub watched: &'a HashSet<String>,
    pub now: i64,
}

struct Kept {
    video: Video,
    why: Vec<String>,
    topic: usize,
    rank: usize,
    score: f64,
}

/// What stands for a video's channel when counting them: the ID, or failing
/// that the name. A video with neither belongs to no channel.
fn channel(v: &Video) -> Option<&str> {
    v.channel_id
        .as_deref()
        .or(v.channel.as_deref())
        .filter(|c| !c.is_empty())
}

/// Filters and orders what the searches found. Nothing here is random: the
/// same finds give the same list.
pub fn rank(reading: &Reading, found: Vec<Found>, ctx: &Context) -> Vec<Ranked> {
    // What the rules keep, once each: under the topic that ranked it highest.
    let mut kept: Vec<Kept> = Vec::new();
    let mut place: HashMap<String, usize> = HashMap::new();
    for f in found {
        if ctx.watched.contains(&f.video.id) {
            continue;
        }
        let subscribed = f
            .video
            .channel_id
            .as_ref()
            .is_some_and(|c| ctx.subscribed.contains(c));
        let Some(reasons) = reading.keeps(&f.video, subscribed) else {
            continue;
        };
        let age_days = f
            .video
            .published
            .map(|p| (ctx.now - p).max(0) as f64 / 86_400.0);
        let score = 1.0 / (1.0 + f.rank as f64)
            + age_days.map_or(0.0, |d| RECENCY * 0.5f64.powf(d / HALF_LIFE))
            + if subscribed { FOLLOWED } else { 0.0 };
        let mut why: Vec<String> = reading.topics.get(f.topic).cloned().into_iter().collect();
        why.extend(reasons);
        let item = Kept {
            video: f.video,
            why,
            topic: f.topic,
            rank: f.rank,
            score,
        };
        match place.get(&item.video.id) {
            Some(&i) if (kept[i].rank, kept[i].topic) <= (item.rank, item.topic) => {}
            Some(&i) => kept[i] = item,
            None => {
                place.insert(item.video.id.clone(), kept.len());
                kept.push(item);
            }
        }
    }

    let ordered = if reading.newest_first {
        kept.sort_by(|a, b| {
            b.video
                .published
                .cmp(&a.video.published)
                .then_with(|| a.video.id.cmp(&b.video.id))
        });
        kept
    } else {
        spread(in_turn(kept))
    };
    ordered
        .into_iter()
        .map(|k| Ranked {
            video: k.video,
            why: k.why,
        })
        .collect()
}

/// Each topic's videos by score, then the topics in turn: the best of the
/// first, the best of the second, and round again, so that a topic with
/// many good results doesn't push the others off the page.
fn in_turn(kept: Vec<Kept>) -> Vec<Kept> {
    let topics = kept.iter().map(|k| k.topic + 1).max().unwrap_or(0);
    let mut by_topic: Vec<Vec<Kept>> = (0..topics).map(|_| Vec::new()).collect();
    for k in kept {
        by_topic[k.topic].push(k);
    }
    for list in &mut by_topic {
        list.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.video.id.cmp(&b.video.id))
        });
        // Taken from the end below.
        list.reverse();
    }
    let mut out = Vec::new();
    while by_topic.iter().any(|l| !l.is_empty()) {
        for list in &mut by_topic {
            out.extend(list.pop());
        }
    }
    out
}

/// Keeps one channel from filling the list: a video that would be the third
/// in a row from its channel, or the fifth from it among the first forty, is
/// held back and tried again after every video that is placed. What is still
/// held at the end follows, in the order it was in.
fn spread(items: Vec<Kept>) -> Vec<Kept> {
    fn fits(placed: &[Kept], item: &Kept) -> bool {
        let Some(c) = channel(&item.video) else {
            return true;
        };
        let same = |k: &Kept| channel(&k.video) == Some(c);
        let n = placed.len();
        let run = n >= IN_A_ROW && placed[n - IN_A_ROW..].iter().all(same);
        let crowded = n < FIRST && placed.iter().filter(|k| same(k)).count() >= PER_CHANNEL;
        !run && !crowded
    }
    let mut placed: Vec<Kept> = Vec::with_capacity(items.len());
    let mut held: Vec<Kept> = Vec::new();
    for item in items {
        if !fits(&placed, &item) {
            held.push(item);
            continue;
        }
        placed.push(item);
        while let Some(i) = held.iter().position(|h| fits(&placed, h)) {
            placed.push(held.remove(i));
        }
    }
    placed.extend(held);
    placed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lens::read::read;

    const NOW: i64 = 1_794_312_000;
    const DAY: i64 = 86_400;

    fn f(id: &str, channel: &str, topic: usize, rank: usize, published: Option<i64>) -> Found {
        Found {
            video: Video {
                id: id.into(),
                title: format!("title {id}"),
                channel: Some(channel.into()),
                channel_id: Some(channel.into()),
                published,
                duration: Some(1900),
                views: None,
                short: false,
                live: false,
            },
            topic,
            rank,
        }
    }

    fn ids(ranked: &[Ranked]) -> Vec<&str> {
        ranked.iter().map(|r| r.video.id.as_str()).collect()
    }

    fn ctx<'a>(subscribed: &'a HashSet<String>, watched: &'a HashSet<String>) -> Context<'a> {
        Context {
            subscribed,
            watched,
            now: NOW,
        }
    }

    #[test]
    fn topics_take_turns_and_a_repeat_is_kept_once() {
        let none = HashSet::new();
        let found = vec![
            f("a", "c1", 0, 0, None),
            f("b", "c2", 0, 1, None),
            f("c", "c3", 0, 2, None),
            f("b", "c2", 1, 0, None),
            f("d", "c4", 1, 1, None),
        ];
        let ranked = rank(&read("x, y", NOW), found, &ctx(&none, &none));
        assert_eq!(ids(&ranked), ["a", "b", "c", "d"]);
        // "b" came higher under the second topic, so that is where it counts.
        assert_eq!(ranked[1].why, ["y"]);
        assert_eq!(ranked[0].why, ["x"]);
    }

    #[test]
    fn newer_and_subscribed_videos_move_up_within_a_topic() {
        let subscribed: HashSet<String> = ["cy".to_string()].into();
        let none = HashSet::new();
        let found = vec![
            f("x", "cx", 0, 0, Some(NOW - 400 * DAY)),
            f("y", "cy", 0, 1, Some(NOW)),
        ];
        let ranked = rank(&read("x", NOW), found.clone(), &ctx(&subscribed, &none));
        assert_eq!(ids(&ranked), ["y", "x"]);
        // Without either, YouTube's own order stands.
        let old = vec![f("x", "cx", 0, 0, None), f("y", "cy", 0, 1, None)];
        assert_eq!(
            ids(&rank(&read("x", NOW), old, &ctx(&none, &none))),
            ["x", "y"]
        );
    }

    #[test]
    fn watched_videos_and_ones_a_rule_turns_away_are_left_out() {
        let none = HashSet::new();
        let watched: HashSet<String> = ["a".to_string()].into();
        let mut short = f("s", "c2", 0, 1, None);
        short.video.short = true;
        let found = vec![f("a", "c1", 0, 0, None), short, f("k", "c3", 0, 2, None)];
        assert_eq!(
            ids(&rank(&read("x", NOW), found, &ctx(&none, &watched))),
            ["k"]
        );
    }

    #[test]
    fn one_channel_gets_neither_three_in_a_row_nor_five_among_the_first_forty() {
        let none = HashSet::new();
        let mut found: Vec<Found> = (0..8)
            .map(|i| f(&format!("a{i}"), "A", 0, i, None))
            .collect();
        found.extend((8..16).map(|i| f(&format!("o{i:02}"), &format!("other{i}"), 0, i, None)));
        let ranked = rank(&read("x", NOW), found, &ctx(&none, &none));
        assert_eq!(
            ids(&ranked),
            [
                "a0", "a1", "o08", "a2", "a3", "o09", "o10", "o11", "o12", "o13", "o14", "o15",
                // What was held back comes last, in its own order.
                "a4", "a5", "a6", "a7"
            ]
        );
    }

    #[test]
    fn a_video_without_a_channel_is_never_held_back() {
        let none = HashSet::new();
        let found: Vec<Found> = (0..5)
            .map(|i| {
                let mut v = f(&format!("n{i}"), "", 0, i, None);
                v.video.channel = None;
                v.video.channel_id = None;
                v
            })
            .collect();
        let ranked = rank(&read("x", NOW), found, &ctx(&none, &none));
        assert_eq!(ids(&ranked), ["n0", "n1", "n2", "n3", "n4"]);
    }

    #[test]
    fn newest_first_ignores_the_rest_of_the_ordering() {
        let none = HashSet::new();
        let found = vec![
            f("p3", "A", 0, 2, Some(3)),
            f("p1", "A", 0, 0, Some(1)),
            f("nd", "A", 0, 1, None),
            f("p2", "A", 1, 0, Some(2)),
        ];
        let ranked = rank(&read("x, y, newest first", NOW), found, &ctx(&none, &none));
        assert_eq!(ids(&ranked), ["p3", "p2", "p1", "nd"]);
    }

    #[test]
    fn why_names_the_topic_and_the_rules_met_and_travels_as_json() {
        let none = HashSet::new();
        let found = vec![f("a", "c1", 0, 0, None)];
        let ranked = rank(&read("talks over 30 min", NOW), found, &ctx(&none, &none));
        assert_eq!(ranked[0].why, ["talks", "over 30:00"]);
        let json = serde_json::to_value(&ranked[0]).unwrap();
        // The video's own fields, with the reasons beside them.
        assert_eq!(json["id"], "a");
        assert_eq!(json["why"], serde_json::json!(["talks", "over 30:00"]));
        let back: Ranked = serde_json::from_value(json).unwrap();
        assert_eq!(back, ranked[0]);
    }
}
