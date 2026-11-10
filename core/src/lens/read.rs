//! Reading a lens: words in, topics and rules out.
//!
//! The reading is shown back to the user as tags before anything is
//! searched, so a rule that was misread is seen, not guessed at. That is
//! also why the rules are few and literal: what can't be shown plainly as a
//! tag isn't read as a rule. English only.

use crate::video::{Video, fmt_duration};
use anyhow::{Result, bail};
use jiff::tz::TimeZone;
use serde::Serialize;

/// How many topics one lens may have: each is a search of its own.
pub const TOPICS_MAX: usize = 6;
/// How long a lens may be, in characters.
pub const TEXT_MAX: usize = 500;

const DAY: i64 = 86_400;
/// What "long" and "short" mean, in seconds. "Long" is YouTube's own line.
const LONG: u64 = 20 * 60;
const SHORT: u64 = 5 * 60;
/// How far back "new" and "recent" reach.
const RECENT: i64 = 30 * DAY;

/// Whether the channels the user follows are wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Subs {
    #[default]
    Any,
    Only,
    Exclude,
}

/// What a lens was understood to say.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Reading {
    /// What to search for, one search each.
    pub topics: Vec<String>,
    /// Words that turn a video away when its title or channel has them.
    pub exclude: Vec<String>,
    /// Seconds.
    pub min_len: Option<u64>,
    pub max_len: Option<u64>,
    /// Unix seconds: nothing published before it.
    pub since: Option<i64>,
    pub shorts: bool,
    pub subs: Subs,
    pub newest_first: bool,
    /// How many topics were left out because there were more than `TOPICS_MAX`.
    pub dropped: usize,
}

/// One thing that was understood, as the window shows it: "about" /
/// "systems talks", "longer than" / "30:00".
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tag {
    pub label: String,
    pub value: String,
}

fn tag(label: &str, value: impl Into<String>) -> Tag {
    Tag {
        label: label.into(),
        value: value.into(),
    }
}

impl Reading {
    /// Whether this is more than a plain search for one thing. A plain
    /// search is handed to YouTube exactly as it was typed.
    pub fn has_rules(&self) -> bool {
        let plain = Reading {
            topics: self.topics.clone(),
            ..Reading::default()
        };
        self.topics.len() != 1 || *self != plain
    }

    /// Whether anything is set besides what to search for. A search without
    /// a filter is one search, however many topics it could be split into,
    /// and goes to YouTube as it was typed ("rick and morty").
    pub fn has_filters(&self) -> bool {
        let plain = Reading {
            topics: self.topics.clone(),
            dropped: self.dropped,
            ..Reading::default()
        };
        *self != plain
    }

    /// A lens has to be about something: rules alone have nothing to search for.
    pub fn check(&self) -> Result<()> {
        if self.topics.is_empty() {
            bail!("Say what it should be about, for example \"woodworking\".");
        }
        Ok(())
    }

    pub fn tags(&self) -> Vec<Tag> {
        let mut tags: Vec<Tag> = self.topics.iter().map(|t| tag("about", t)).collect();
        tags.extend(self.exclude.iter().map(|x| tag("not", x)));
        if let Some(min) = self.min_len {
            tags.push(tag("longer than", fmt_duration(min)));
        }
        if let Some(max) = self.max_len {
            tags.push(tag("shorter than", fmt_duration(max)));
        }
        if let Some(since) = self.since {
            tags.push(tag("since", fmt_date(since)));
        }
        if self.shorts {
            tags.push(tag("shorts", "included"));
        }
        match self.subs {
            Subs::Any => {}
            Subs::Only => tags.push(tag("only", "channels you follow")),
            Subs::Exclude => tags.push(tag("not", "channels you follow")),
        }
        if self.newest_first {
            tags.push(tag("order", "newest first"));
        }
        match self.dropped {
            0 => {}
            1 => tags.push(tag("ignored", "1 more topic")),
            n => tags.push(tag("ignored", format!("{n} more topics"))),
        }
        tags
    }

    /// The reasons `v` is kept, or `None` when a rule turns it away. A rule
    /// that can't be checked (a length rule and a video of unknown length)
    /// turns the video away: what is shown must be known to fit.
    pub fn keeps(&self, v: &Video, subscribed: bool) -> Option<Vec<String>> {
        if v.live || (v.short && !self.shorts) {
            return None;
        }
        let mut why = Vec::new();
        if let Some(min) = self.min_len {
            if v.duration? < min {
                return None;
            }
            why.push(format!("over {}", fmt_duration(min)));
        }
        if let Some(max) = self.max_len {
            if v.duration? > max {
                return None;
            }
            why.push(format!("under {}", fmt_duration(max)));
        }
        if let Some(since) = self.since {
            if v.published? < since {
                return None;
            }
            why.push(format!("since {}", fmt_date(since)));
        }
        // By whole words: "no ai" must not turn away "Explained".
        let title = words(&v.title);
        let channel = words(v.channel.as_deref().unwrap_or_default());
        if self.exclude.iter().any(|x| {
            let x = words(x);
            has_run(&title, &x) || has_run(&channel, &x)
        }) {
            return None;
        }
        match self.subs {
            Subs::Only if !subscribed => return None,
            Subs::Exclude if subscribed => return None,
            Subs::Only => why.push("a channel you follow".into()),
            _ => {}
        }
        Some(why)
    }
}

/// The words of `text`, in lower case: runs of letters and digits.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// Whether the words of `phrase` occur in `text`, next to each other and in order.
fn has_run(text: &[String], phrase: &[String]) -> bool {
    !phrase.is_empty() && text.windows(phrase.len()).any(|run| run == phrase)
}

/// "1 Jan 2026", in UTC.
fn fmt_date(ts: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let Ok(t) = jiff::Timestamp::from_second(ts) else {
        return String::new();
    };
    let d = t.to_zoned(TimeZone::UTC).date();
    format!(
        "{} {} {}",
        d.day(),
        MONTHS[(d.month() - 1) as usize],
        d.year()
    )
}

/// Midnight (UTC) on the first day of `now`'s year, or of its month.
fn start_of(now: i64, month: bool) -> Option<i64> {
    let d = jiff::Timestamp::from_second(now)
        .ok()?
        .to_zoned(TimeZone::UTC)
        .date();
    let first = jiff::civil::date(d.year(), if month { d.month() } else { 1 }, 1);
    Some(first.to_zoned(TimeZone::UTC).ok()?.timestamp().as_second())
}

fn start_of_year(year: i16) -> Option<i64> {
    let first = jiff::civil::Date::new(year, 1, 1).ok()?;
    Some(first.to_zoned(TimeZone::UTC).ok()?.timestamp().as_second())
}

/// Who the text is for, which decides how freely it is read.
#[derive(Clone, Copy, PartialEq)]
enum For {
    /// A lens: its reading is shown as tags and saved by choice, so it can
    /// say what it leaves out in words ("no music").
    Lens,
    /// The search box: most of what is typed there is a title or a name, so
    /// "no …" is only words ("no man's sky"), and a minus sign is how
    /// something is left out.
    Search,
}

/// Reads `text` as a lens, at the time `now` (Unix seconds), which the date
/// rules are measured from.
pub fn read(text: &str, now: i64) -> Reading {
    read_for(text, now, For::Lens)
}

/// Reads `text` as a search.
pub fn read_search(text: &str, now: i64) -> Reading {
    read_for(text, now, For::Search)
}

fn read_for(text: &str, now: i64, whom: For) -> Reading {
    let mut r = Reading::default();
    let mut topics: Vec<String> = Vec::new();
    for part in parts(text) {
        match part {
            // Whatever is in quotes is searched for as it stands.
            Part::Quoted(q) => {
                let q = q.trim();
                if !q.is_empty() {
                    topics.push(q.into());
                }
            }
            Part::Plain(p) => {
                for clause in clauses(p) {
                    topics.extend(read_clause(clause, now, whom, &mut r));
                }
            }
        }
    }
    r.dropped = topics.len().saturating_sub(TOPICS_MAX);
    topics.truncate(TOPICS_MAX);
    r.topics = topics;
    r
}

enum Part<'a> {
    Plain(&'a str),
    Quoted(&'a str),
}

/// Splits at pairs of double quotes. A quote without a partner is an
/// ordinary character (inches, say).
fn parts(text: &str) -> Vec<Part<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('"') {
        let Some(len) = rest[open + 1..].find('"') else {
            break;
        };
        out.push(Part::Plain(&rest[..open]));
        out.push(Part::Quoted(&rest[open + 1..open + 1 + len]));
        rest = &rest[open + len + 2..];
    }
    out.push(Part::Plain(rest));
    out
}

/// The clauses of unquoted text: split at commas, semicolons, new lines and
/// the word "and". Each is a list of words.
fn clauses(text: &str) -> Vec<Vec<&str>> {
    let mut out = Vec::new();
    for piece in text.split([',', ';', '\n']) {
        let mut clause = Vec::new();
        for word in piece.split_whitespace() {
            if word.eq_ignore_ascii_case("and") {
                out.push(std::mem::take(&mut clause));
            } else {
                clause.push(word);
            }
        }
        out.push(clause);
    }
    out.retain(|c| !c.is_empty());
    out
}

/// The words of a clause, with their lower-case forms beside them, so that
/// rules are matched without regard to case and topics keep theirs.
struct Words {
    as_typed: Vec<String>,
    lower: Vec<String>,
}

impl Words {
    fn new(words: &[&str]) -> Self {
        Words {
            as_typed: words.iter().map(|w| w.to_string()).collect(),
            lower: words.iter().map(|w| w.to_ascii_lowercase()).collect(),
        }
    }

    fn len(&self) -> usize {
        self.lower.len()
    }

    fn at(&self, i: usize) -> &str {
        self.lower.get(i).map(String::as_str).unwrap_or_default()
    }

    /// Whether the words from `i` on begin with `phrase`.
    fn has(&self, i: usize, phrase: &[&str]) -> bool {
        phrase.iter().enumerate().all(|(k, p)| self.at(i + k) == *p)
    }

    fn is(&self, phrases: &[&str]) -> bool {
        let all = self.lower.join(" ");
        phrases.contains(&all.as_str())
    }

    fn remove(&mut self, from: usize, count: usize) {
        let to = (from + count).min(self.len());
        self.as_typed.drain(from..to);
        self.lower.drain(from..to);
    }

    /// Removes `count` words at `from`, and the small words that led up to
    /// them: in "from the last 3 months", "from the" goes with the date.
    fn lift(&mut self, from: usize, count: usize) {
        self.remove(from, count);
        let mut from = from;
        while from > 0 && matches!(self.at(from - 1), "from" | "in" | "the" | "of" | "within") {
            from -= 1;
            self.remove(from, 1);
        }
    }
}

/// Reads one clause into `r`, and returns the topic it leaves, if any.
fn read_clause(clause: Vec<&str>, now: i64, whom: For, r: &mut Reading) -> Option<String> {
    let mut w = Words::new(&clause);

    // "-word" anywhere: without that word.
    let mut i = 0;
    while i < w.len() {
        let minus = w.as_typed[i]
            .strip_prefix('-')
            .filter(|x| x.chars().next().is_some_and(char::is_alphabetic))
            .map(String::from);
        match minus {
            Some(word) => {
                if word.eq_ignore_ascii_case("shorts") {
                    r.shorts = false;
                } else {
                    r.exclude.push(word);
                }
                w.remove(i, 1);
            }
            None => i += 1,
        }
    }
    if w.len() == 0 {
        return None;
    }

    // "no …": the whole clause is what isn't wanted.
    if whom == For::Lens && w.len() > 1 && matches!(w.at(0), "no" | "not" | "without" | "except") {
        if w.len() == 2 && w.at(1) == "shorts" {
            r.shorts = false;
        } else if w.has(1, &["from", "my", "subscriptions"]) {
            r.subs = Subs::Exclude;
        } else {
            r.exclude.push(w.as_typed[1..].join(" "));
        }
        return None;
    }

    if w.is(&[
        "shorts",
        "with shorts",
        "including shorts",
        "include shorts",
    ]) {
        r.shorts = true;
        return None;
    }
    if w.is(&[
        "from my subscriptions",
        "only my subscriptions",
        "only from my subscriptions",
        "channels i follow",
        "only channels i follow",
    ]) {
        r.subs = Subs::Only;
        return None;
    }
    if w.is(&["newest first"]) {
        r.newest_first = true;
        return None;
    }

    // One word that is a rule, and only as a clause of its own: "jazz, new"
    // is jazz from the last month, "new york" is a city.
    if w.len() == 1 {
        match w.at(0) {
            "long" | "long-form" | "longform" => r.min_len = Some(LONG),
            "short" | "quick" => r.max_len = Some(SHORT),
            "today" => r.since = Some(now - DAY),
            "new" | "recent" => r.since = Some(now - RECENT),
            _ => return Some(w.as_typed.join(" ")),
        }
        return None;
    }

    lift_lengths(&mut w, r);
    lift_dates(&mut w, now, r);

    // What is left is the topic, without the words people wrap one in.
    loop {
        let lead = [
            &["show", "me"][..],
            &["i", "want"],
            &["videos", "about"],
            &["videos", "on"],
            &["about"],
        ]
        .into_iter()
        .find(|p| w.has(0, p));
        match lead {
            Some(p) => w.remove(0, p.len()),
            None => break,
        }
    }
    if w.len() > 0 && w.at(w.len() - 1) == "videos" {
        w.remove(w.len() - 1, 1);
    }
    (w.len() > 0).then(|| w.as_typed.join(" "))
}

/// A number of seconds from "30 min" (two words) or "90min" (one), and how
/// many words that was.
fn amount(w: &Words, i: usize) -> Option<(u64, usize)> {
    let first = w.at(i);
    let digits = first
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(first.len());
    let n: f64 = first[..digits].parse().ok()?;
    let (unit, used) = if digits < first.len() {
        (&first[digits..], 1)
    } else {
        (w.at(i + 1), 2)
    };
    let each = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
        "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
        _ => return None,
    };
    let secs = n * each;
    (secs.is_finite() && secs >= 0.0).then_some((secs.round() as u64, used))
}

fn lift_lengths(w: &mut Words, r: &mut Reading) {
    const MORE: [&[&str]; 4] = [
        &["longer", "than"],
        &["more", "than"],
        &["at", "least"],
        &["over"],
    ];
    const LESS: [&[&str]; 4] = [
        &["shorter", "than"],
        &["less", "than"],
        &["at", "most"],
        &["under"],
    ];
    let mut i = 0;
    while i < w.len() {
        // "over 30 min", "at most 2 hours": the words, then a length.
        let said = MORE
            .iter()
            .map(|p| (p, true))
            .chain(LESS.iter().map(|p| (p, false)))
            .find(|(p, _)| w.has(i, p));
        if let Some((phrase, more)) = said
            && let Some((secs, used)) = amount(w, i + phrase.len())
        {
            if more {
                r.min_len = Some(secs);
            } else {
                r.max_len = Some(secs);
            }
            w.lift(i, phrase.len() + used);
            continue;
        }
        // "10-20 min".
        if let Some((from, to)) = w.at(i).split_once('-')
            && let Ok(from) = from.parse::<f64>()
        {
            let probe = Words::new(&[to, w.at(i + 1)]);
            if let Some((secs, used)) = amount(&probe, 0) {
                // The unit belongs to both ends.
                let each = secs as f64 / to_number(to);
                r.min_len = Some((from * each).round() as u64);
                r.max_len = Some(secs);
                w.lift(i, used);
                continue;
            }
        }
        i += 1;
    }
}

/// The number a length starts with ("20" of "20min").
fn to_number(word: &str) -> f64 {
    let digits = word
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(word.len());
    word[..digits].parse().unwrap_or(1.0)
}

fn lift_dates(w: &mut Words, now: i64, r: &mut Reading) {
    let mut i = 0;
    while i < w.len() {
        let found = if w.at(i) == "this" {
            match w.at(i + 1) {
                "week" => Some((now - 7 * DAY, 2)),
                "month" => start_of(now, true).map(|t| (t, 2)),
                "year" => start_of(now, false).map(|t| (t, 2)),
                _ => None,
            }
        } else if matches!(w.at(i), "past" | "last") {
            let days = match w.at(i + 2) {
                "day" | "days" => Some(1),
                "week" | "weeks" => Some(7),
                "month" | "months" => Some(30),
                "year" | "years" => Some(365),
                _ => None,
            };
            w.at(i + 1)
                .parse::<i64>()
                .ok()
                .filter(|n| (1..=10_000).contains(n))
                .zip(days)
                .map(|(n, each)| (now - n * each * DAY, 3))
        } else if w.at(i) == "since" {
            w.at(i + 1)
                .parse::<i16>()
                .ok()
                .filter(|y| (1990..=2100).contains(y))
                .and_then(start_of_year)
                .map(|t| (t, 2))
        } else {
            None
        };
        match found {
            // Only where it ends the clause: "talks this year" is a date,
            // "this week in tech" is a programme.
            Some((since, used)) if i + used == w.len() => {
                r.since = Some(since);
                w.lift(i, used);
            }
            _ => i += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-10 12:00:00 UTC.
    const NOW: i64 = 1_794_312_000;
    /// 2026-01-01 00:00:00 UTC.
    const JAN1: i64 = 1_767_225_600;
    const DAY: i64 = 86_400;

    fn topics(text: &str) -> Vec<String> {
        read(text, NOW).topics
    }

    #[test]
    fn the_examples_from_the_design_read_as_drawn() {
        let r = read(
            "slow woodworking with hand tools, no music, longer than 15 minutes, from this year",
            NOW,
        );
        assert_eq!(r.topics, ["slow woodworking with hand tools"]);
        assert_eq!(r.exclude, ["music"]);
        assert_eq!(
            (r.min_len, r.max_len, r.since),
            (Some(900), None, Some(JAN1))
        );
        let r = read("systems talks over 30 min this year -shorts", NOW);
        assert_eq!(r.topics, ["systems talks"]);
        assert_eq!(
            (r.min_len, r.since, r.shorts),
            (Some(1800), Some(JAN1), false)
        );
        assert!(r.has_rules());
    }

    #[test]
    fn a_plain_search_has_no_rules_and_quotes_are_literal() {
        for q in [
            "rust",
            "Rust in 100 Seconds",
            "how to tie a bowline",
            "game over",
        ] {
            let r = read(q, NOW);
            assert_eq!(r.topics, [q]);
            assert!(!r.has_rules(), "{q}");
        }
        let r = read(r#""no country for old men, over 9000""#, NOW);
        assert_eq!(r.topics, ["no country for old men, over 9000"]);
        assert!(!r.has_rules());
        // Rules still count outside the quotes.
        let r = read(r#""long way round" over 30 min"#, NOW);
        assert_eq!(r.topics, ["long way round"]);
        assert_eq!(r.min_len, Some(1800));
        // A quote that never closes is just a character.
        assert_eq!(topics(r#"5" pipe fitting"#), [r#"5" pipe fitting"#]);
    }

    #[test]
    fn lengths_dates_and_the_other_rules() {
        let r = read("10-20 min synth repair", NOW);
        assert_eq!((r.min_len, r.max_len), (Some(600), Some(1200)));
        assert_eq!(r.topics, ["synth repair"]);
        assert_eq!(read("knots, under 5 minutes", NOW).max_len, Some(300));
        assert_eq!(read("lectures at least 1.5 h", NOW).min_len, Some(5400));
        assert_eq!(read("lectures over 90min", NOW).min_len, Some(5400));
        assert_eq!(read("clips shorter than 45 seconds", NOW).max_len, Some(45));
        let r = read("interviews, long-form", NOW);
        assert_eq!(
            (r.topics.as_slice(), r.min_len),
            (&["interviews".to_string()][..], Some(1200))
        );
        let r = read("recipes, quick", NOW);
        assert_eq!(
            (r.topics.as_slice(), r.max_len),
            (&["recipes".to_string()][..], Some(300))
        );
        assert_eq!(read("cats, past 2 weeks", NOW).since, Some(NOW - 14 * DAY));
        assert_eq!(
            read("cats from the last 3 months", NOW).since,
            Some(NOW - 90 * DAY)
        );
        assert_eq!(topics("cats from the last 3 months"), ["cats"]);
        assert_eq!(read("cats since 2024", NOW).since, Some(1_704_067_200));
        assert_eq!(read("cats, today", NOW).since, Some(NOW - DAY));
        assert_eq!(read("cats this week", NOW).since, Some(NOW - 7 * DAY));
        // 2026-11-01 00:00:00 UTC.
        assert_eq!(read("cats this month", NOW).since, Some(1_793_491_200));
        assert_eq!(read("jazz, new", NOW).since, Some(NOW - 30 * DAY));
        assert_eq!(topics("jazz, new"), ["jazz"]);
        assert_eq!(read("jazz, from my subscriptions", NOW).subs, Subs::Only);
        assert_eq!(
            read("jazz, not from my subscriptions", NOW).subs,
            Subs::Exclude
        );
        assert!(read("jazz, newest first", NOW).newest_first);
        assert!(read("jazz, with shorts", NOW).shorts);
        assert_eq!(
            topics("show me videos about bread and sourdough videos"),
            ["bread", "sourdough"]
        );
        assert_eq!(read("jazz -fusion -live", NOW).exclude, ["fusion", "live"]);
        assert_eq!(
            read("jazz, without vocals or lyrics", NOW).exclude,
            ["vocals or lyrics"]
        );
    }

    #[test]
    fn a_word_in_the_middle_of_a_phrase_is_not_a_rule() {
        let egg = read("how long to boil an egg", NOW);
        assert_eq!(egg.topics, ["how long to boil an egg"]);
        assert_eq!(egg.min_len, None);
        assert!(!egg.has_rules());
        assert!(!read("what's new in rust", NOW).has_rules());
        assert!(!read("the short history of nearly everything", NOW).has_rules());
        // "over" and "under" need a length after them.
        assert!(!read("under the bridge", NOW).has_rules());
        assert!(!read("over 9000", NOW).has_rules());
    }

    #[test]
    fn names_and_titles_that_hold_a_rule_word_are_left_whole() {
        // One-word rules only count as a clause of their own, and a date
        // only where it ends its clause.
        for q in [
            "new york",
            "long island iced tea",
            "the big short",
            "short film",
            "quick sort",
            "usa today",
            "this week in tech",
            "today show highlights",
            "recent discoveries in physics",
        ] {
            let r = read(q, NOW);
            assert_eq!(r.topics, [q], "{q}");
            assert!(!r.has_filters(), "{q}");
        }
    }

    #[test]
    fn a_search_is_only_handled_here_when_it_has_a_filter() {
        // Several topics are still one search when that is all there is:
        // the search box sends such a query on as it was typed.
        for q in [
            "rick and morty",
            "Paris, Texas",
            r#"rust "borrow checker" explained"#,
        ] {
            assert!(!read_search(q, NOW).has_filters(), "{q}");
        }
        // In a search, "no …" is words like any other: it is how titles
        // begin. Only a minus sign excludes.
        let r = read_search("no man's sky, base building", NOW);
        assert!(!r.has_filters());
        assert!(r.exclude.is_empty());
        assert_eq!(read_search("jazz -fusion", NOW).exclude, ["fusion"]);
        assert!(read_search("talks over 30 min", NOW).has_filters());
        // A lens says what it leaves out in words.
        assert_eq!(read("jazz, no fusion", NOW).exclude, ["fusion"]);
    }

    #[test]
    fn what_is_left_out_is_matched_by_whole_words() {
        let v = |title: &str| Video {
            id: "aaaaaaaaaaa".into(),
            title: title.into(),
            channel: Some("Oliver Cooks".into()),
            channel_id: None,
            published: None,
            duration: None,
            views: None,
            short: false,
            live: false,
        };
        let r = read("cooking, no ai", NOW);
        assert!(
            r.keeps(&v("Sourdough Explained, Again: Training a Starter"), false)
                .is_some()
        );
        assert!(r.keeps(&v("AI cooking"), false).is_none());
        assert!(r.keeps(&v("Cooking (ai)"), false).is_none());
        // The channel is called Oliver: "-live" doesn't mean it.
        assert!(
            read("cooking -live", NOW)
                .keeps(&v("Dinner"), false)
                .is_some()
        );
        assert!(
            read("cooking -live", NOW)
                .keeps(&v("Dinner, LIVE!"), false)
                .is_none()
        );
        // A phrase is matched as that run of words.
        let r = read("cooking, no pressure cooker", NOW);
        assert!(r.keeps(&v("Pressure and a cooker"), false).is_some());
        assert!(r.keeps(&v("My Pressure Cooker stew"), false).is_none());
    }

    #[test]
    fn only_six_topics_are_read_and_rules_alone_are_refused() {
        let r = read("a1, b2, c3, d4, e5, f6, g7, h8", NOW);
        assert_eq!((r.topics.len(), r.dropped), (6, 2));
        assert_eq!(
            r.tags().last().unwrap(),
            &Tag {
                label: "ignored".into(),
                value: "2 more topics".into()
            }
        );
        let r = read("no music, over 10 min", NOW);
        assert!(r.topics.is_empty());
        assert!(
            r.check()
                .unwrap_err()
                .to_string()
                .starts_with("Say what it should be about")
        );
        assert!(read("bread", NOW).check().is_ok());
        assert!(read("", NOW).check().is_err());
    }

    #[test]
    fn the_tags_say_what_was_understood() {
        let said = |text: &str| -> Vec<String> {
            read(text, NOW)
                .tags()
                .iter()
                .map(|t| format!("{} {}", t.label, t.value))
                .collect()
        };
        assert_eq!(
            said("systems talks over 30 min this year, no drama"),
            [
                "about systems talks",
                "not drama",
                "longer than 30:00",
                "since 1 Jan 2026"
            ]
        );
        assert_eq!(
            said("jazz under 2 hours, with shorts, from my subscriptions, newest first"),
            [
                "about jazz",
                "shorter than 2:00:00",
                "shorts included",
                "only channels you follow",
                "order newest first"
            ]
        );
        assert_eq!(
            said("jazz, not from my subscriptions"),
            ["about jazz", "not channels you follow"]
        );
    }

    #[test]
    fn a_video_is_kept_or_turned_away_by_each_rule() {
        let r = read("talks over 30 min this year, no drama", NOW);
        let v = |title: &str, dur: Option<u64>, published: Option<i64>| Video {
            id: "aaaaaaaaaaa".into(),
            title: title.into(),
            channel: Some("Chan".into()),
            channel_id: None,
            published,
            duration: dur,
            views: None,
            short: false,
            live: false,
        };
        assert_eq!(
            r.keeps(&v("A talk", Some(1900), Some(NOW - 5)), false)
                .unwrap(),
            ["over 30:00", "since 1 Jan 2026"]
        );
        assert!(
            r.keeps(&v("A talk", Some(1700), Some(NOW - 5)), false)
                .is_none()
        );
        assert!(r.keeps(&v("A talk", None, Some(NOW - 5)), false).is_none());
        assert!(
            r.keeps(&v("A talk", Some(1900), Some(JAN1 - 1)), false)
                .is_none()
        );
        assert!(r.keeps(&v("A talk", Some(1900), None), false).is_none());
        assert!(
            r.keeps(&v("Conference DRAMA", Some(1900), Some(NOW - 5)), false)
                .is_none()
        );
        let only = read("jazz, from my subscriptions", NOW);
        assert!(only.keeps(&v("Jazz", None, None), false).is_none());
        assert_eq!(
            only.keeps(&v("Jazz", None, None), true).unwrap(),
            ["a channel you follow"]
        );
        let others = read("jazz, not from my subscriptions", NOW);
        assert!(others.keeps(&v("Jazz", None, None), true).is_none());
        assert!(
            others
                .keeps(&v("Jazz", None, None), false)
                .unwrap()
                .is_empty()
        );
        // Shorts and live streams.
        let plain = read("jazz", NOW);
        let mut short = v("Jazz", Some(40), None);
        short.short = true;
        assert!(plain.keeps(&short, false).is_none());
        assert!(
            read("jazz, with shorts", NOW)
                .keeps(&short, false)
                .is_some()
        );
        let mut live = v("Jazz", None, None);
        live.live = true;
        assert!(plain.keeps(&live, false).is_none());
        // The channel's name counts for what is excluded.
        let no_chan = read("jazz, no chan", NOW);
        assert!(no_chan.keeps(&v("Jazz", None, None), false).is_none());
        let under = read("jazz under 5 min", NOW);
        assert_eq!(
            under.keeps(&v("Jazz", Some(200), None), false).unwrap(),
            ["under 5:00"]
        );
        assert!(under.keeps(&v("Jazz", Some(400), None), false).is_none());
    }
}
