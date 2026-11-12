use serde::{Deserialize, Deserializer, Serialize};

/// Where a live stream is: running, or announced for later. A stream that has
/// finished is an ordinary video, marked by `Video::streamed`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LiveState {
    Now,
    Upcoming,
}

/// Reads `live` as a state, or as the flag that older caches hold. The flag
/// didn't say which, so `true` counts as running; the next refresh replaces it.
fn state_or_flag<'de, D: Deserializer<'de>>(d: D) -> Result<Option<LiveState>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Flag(bool),
        State(LiveState),
    }
    Ok(match Option::<Stored>::deserialize(d)? {
        Some(Stored::State(s)) => Some(s),
        Some(Stored::Flag(true)) => Some(LiveState::Now),
        Some(Stored::Flag(false)) | None => None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Video {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub views: Option<u64>,
    #[serde(default)]
    pub short: bool,
    /// Running (`"now"`) or announced (`"upcoming"`); absent for an ordinary
    /// video. Only written when set, which keeps old caches readable and the
    /// API's JSON the same for every ordinary video.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "state_or_flag"
    )]
    pub live: Option<LiveState>,
    /// When an upcoming stream is due to start, in unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts: Option<i64>,
    /// How many are watching a stream that is running, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watching: Option<u64>,
    /// A stream that has finished (`was_live`, `post_live`): an ordinary
    /// video that was streamed. Only written when true.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub streamed: bool,
}

/// A chapter of a video: where it starts, in seconds, and what it is called.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chapter {
    pub start: u64,
    pub title: String,
}

pub fn fmt_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn fmt_age(ts: i64, now: i64) -> String {
    let d = (now - ts).max(0);
    match d {
        _ if d < 3600 => format!("{}m ago", d / 60),
        _ if d < 86_400 => format!("{}h ago", d / 3600),
        _ if d < 86_400 * 60 => format!("{}d ago", d / 86_400),
        _ if d < 86_400 * 730 => format!("{}mo ago", d / (86_400 * 30)),
        _ => format!("{}y ago", d / (86_400 * 365)),
    }
}

pub fn fmt_count(n: u64) -> String {
    match n {
        _ if n >= 1_000_000_000 => format!("{:.1}B", n as f64 / 1e9),
        _ if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ if n >= 1_000 => format!("{:.0}K", n as f64 / 1e3),
        _ => n.to_string(),
    }
}

impl Video {
    /// A single line for listing. `date` overrides `published` (feed dates may be estimated).
    pub fn line(&self, date: Option<(i64, bool)>, now: i64) -> String {
        let mut parts = vec![self.title.replace(['\t', '\n'], " ")];
        if let Some(c) = &self.channel {
            parts.push(c.clone());
        }
        if self.live.is_some() {
            parts.push("[live]".into());
        } else if self.short {
            parts.push("short".into());
        } else if let Some(d) = self.duration {
            parts.push(fmt_duration(d));
        }
        if let Some(v) = self.views {
            parts.push(format!("{} views", fmt_count(v)));
        }
        match date.or(self.published.map(|p| (p, false))) {
            Some((0, _)) => {}
            Some((t, est)) => {
                parts.push(format!("{}{}", if est { "~" } else { "" }, fmt_age(t, now)))
            }
            None => {}
        }
        parts.join("  ·  ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(fmt_duration(149), "2:29");
        assert_eq!(fmt_duration(3723), "1:02:03");
        assert_eq!(fmt_age(1000, 1000 + 7200), "2h ago");
        assert_eq!(fmt_count(2_648_797), "2.6M");
    }

    fn video(live: Option<LiveState>) -> Video {
        Video {
            id: "dQw4w9WgXcQ".into(),
            title: "T".into(),
            channel: Some("C".into()),
            channel_id: None,
            published: None,
            duration: Some(90),
            views: None,
            short: false,
            live,
            starts: None,
            watching: None,
            streamed: false,
        }
    }

    #[test]
    fn live_state_serialises_as_words() {
        let now = serde_json::to_value(video(Some(LiveState::Now))).unwrap();
        assert_eq!(now["live"], "now");
        let soon = serde_json::to_value(video(Some(LiveState::Upcoming))).unwrap();
        assert_eq!(soon["live"], "upcoming");
        let plain = serde_json::to_value(video(None)).unwrap();
        for key in ["live", "starts", "watching", "streamed"] {
            assert!(plain.get(key).is_none(), "{key} in {plain}");
        }
        let mut over = video(None);
        over.streamed = true;
        assert_eq!(serde_json::to_value(over).unwrap()["streamed"], true);
    }

    #[test]
    fn a_status_round_trips() {
        let mut v = video(Some(LiveState::Upcoming));
        v.starts = Some(1_800_000_000);
        v.watching = Some(12);
        let back: Video = serde_json::from_value(serde_json::to_value(&v).unwrap()).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn old_caches_still_load() {
        // Written before the field existed.
        let old: Video = serde_json::from_str(r#"{"id":"dQw4w9WgXcQ","title":"T"}"#).unwrap();
        assert_eq!(old.live, None);
        assert!(!old.streamed);
        // Written when `live` was a flag: a stream that was on then.
        let flagged: Video =
            serde_json::from_str(r#"{"id":"dQw4w9WgXcQ","title":"T","live":true}"#).unwrap();
        assert_eq!(flagged.live, Some(LiveState::Now));
        let null: Video =
            serde_json::from_str(r#"{"id":"dQw4w9WgXcQ","title":"T","live":null}"#).unwrap();
        assert_eq!(null.live, None);
    }

    #[test]
    fn a_listing_marks_live_streams() {
        assert_eq!(
            video(Some(LiveState::Now)).line(None, 0),
            "T  ·  C  ·  [live]"
        );
        assert_eq!(
            video(Some(LiveState::Upcoming)).line(None, 0),
            "T  ·  C  ·  [live]"
        );
        assert_eq!(video(None).line(None, 0), "T  ·  C  ·  1:30");
    }
}
