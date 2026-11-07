use serde::{Deserialize, Serialize};

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
    /// A live stream, or one that hasn't started yet (yt-dlp's `is_live` or
    /// `is_upcoming`). qveil can't play those, so the window hides them.
    /// Only written when true, which keeps old caches readable and the
    /// API's JSON the same for every ordinary video.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub live: bool,
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
        if self.live {
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

    fn video(live: bool) -> Video {
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
        }
    }

    #[test]
    fn live_is_only_in_the_json_when_true_and_old_caches_still_load() {
        let plain = serde_json::to_value(video(false)).unwrap();
        assert!(plain.get("live").is_none(), "{plain}");
        assert_eq!(serde_json::to_value(video(true)).unwrap()["live"], true);
        // A cache written before the field existed.
        let old: Video = serde_json::from_str(r#"{"id":"dQw4w9WgXcQ","title":"T"}"#).unwrap();
        assert!(!old.live);
    }

    #[test]
    fn a_listing_marks_live_streams() {
        assert_eq!(video(true).line(None, 0), "T  ·  C  ·  [live]");
        assert_eq!(video(false).line(None, 0), "T  ·  C  ·  1:30");
    }
}
