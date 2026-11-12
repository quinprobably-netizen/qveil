//! A live stream, or a past one that is only offered as DASH fragments, as
//! the engine plays it: one video track and one audio track, each a list of
//! fragments fetched over the route, never a manifest handed to the player.

// What a stream opens as is read by the playlists and the play path, which
// come after the lookup.
#![cfg_attr(not(test), allow(dead_code))]

use crate::ids;
use crate::ytdlp::{Streams, relay_headers, str_field};
use anyhow::{Context, Result};
use serde_json::Value;

/// One track of a live stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// The fragments' common start: `base&sq=n` for `Fragments::Numbered`,
    /// `base` + a listed path for `Fragments::Listed`.
    pub base: String,
    /// What a segment link ends in. FFmpeg only takes known extensions, and
    /// `webm` is not one: a WebM video is `mkv` (WebM is Matroska), an MP4
    /// one `mp4`, and audio `m4a`.
    pub ext: &'static str,
    pub format_id: String,
}

/// Which fragments a track has.
#[derive(Clone, Debug, PartialEq)]
pub enum Fragments {
    /// A running stream: fragment `n` is `base&sq=n`, up to the head.
    Numbered,
    /// A past broadcast: DASH `fragment_base_url` + each `fragments[].path`,
    /// with its duration in seconds.
    Listed(Vec<(String, f64)>),
}

#[derive(Clone, Debug)]
pub struct LiveStreams {
    pub title: String,
    pub channel: Option<String>,
    pub channel_id: Option<String>,
    pub video: Track,
    pub audio: Track,
    pub fragments: Fragments,
    /// How long a fragment is, in seconds (yt-dlp's `target_duration`).
    pub target: f64,
    /// When the stream started, in Unix seconds.
    pub started: Option<i64>,
    pub watching: Option<u64>,
    /// The stream is over (`was_live`, `post_live`).
    pub ended: bool,
    /// Whether fragments older than the head are offered. Assumed here, and
    /// corrected once the timeline finds out.
    pub rewind: bool,
    /// The headers the fragments are fetched with, cookies left out.
    pub headers: Vec<(String, String)>,
}

/// What a video opens as.
#[derive(Clone, Debug)]
pub enum Opening {
    /// An ordinary video, a plain download.
    Plain(Streams),
    /// A running stream, or one only offered as fragments.
    Live(LiveStreams),
    /// An announced stream that hasn't started.
    Upcoming {
        title: String,
        channel: Option<String>,
        starts: Option<i64>,
    },
}

/// The best video within `max_height`, and the best audio, from a live
/// lookup's answer. yt-dlp orders `formats` worst to best, so each is the
/// last of its kind that can be fetched in fragments.
pub fn choose(v: &Value, max_height: u32) -> Result<LiveStreams> {
    let formats = v["formats"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let only = |f: &Value, key: &str| f[key].as_str().is_some_and(|c| c != "none");
    let (video_fmt, video, fragments) = formats
        .iter()
        .rev()
        .filter(|f| only(f, "vcodec") && !only(f, "acodec"))
        .filter(|f| {
            f["height"]
                .as_u64()
                .is_some_and(|h| h <= u64::from(max_height))
        })
        .find_map(|f| {
            let ext = match f["ext"].as_str()? {
                "webm" => "mkv",
                "mp4" => "mp4",
                _ => return None,
            };
            let (base, fragments) = fragments_of(f)?;
            Some((f, track(f, base, ext)?, fragments))
        })
        .context(NO_FRAGMENTS)?;
    // The audio has to be cut the same way: one list of fragments serves both.
    let audio = formats
        .iter()
        .rev()
        .filter(|f| only(f, "acodec") && !only(f, "vcodec"))
        .find_map(|f| {
            let (base, theirs) = fragments_of(f)?;
            same_cut(&fragments, &theirs).then(|| track(f, base, "m4a"))?
        })
        .context(NO_FRAGMENTS)?;
    let target = video_fmt["target_duration"]
        .as_f64()
        .or_else(|| match &fragments {
            Fragments::Listed(list) => list.iter().map(|(_, d)| *d).reduce(f64::max),
            Fragments::Numbered => None,
        })
        .filter(|t| *t > 0.0)
        .context(NO_FRAGMENTS)?;
    Ok(LiveStreams {
        title: str_field(v, &["title", "id"]).unwrap_or_default(),
        channel: str_field(v, &["channel", "uploader"]),
        channel_id: str_field(v, &["channel_id"]).filter(|c| ids::is_channel_id(c)),
        headers: relay_headers(video_fmt),
        video,
        audio,
        fragments,
        target,
        started: v["release_timestamp"].as_i64().or(v["timestamp"].as_i64()),
        watching: v["concurrent_view_count"].as_u64(),
        ended: matches!(v["live_status"].as_str(), Some("was_live" | "post_live")),
        rewind: true,
    })
}

/// What a live stream without a track that can be fetched in fragments ends
/// with: an HLS-only stream, or one without formats at all.
pub const NO_FRAGMENTS: &str =
    "This live stream offers no fragments that can be fetched through your route.";

fn track(f: &Value, base: String, ext: &'static str) -> Option<Track> {
    Some(Track {
        base,
        ext,
        format_id: f["format_id"].as_str()?.to_string(),
    })
}

/// A format's fragments and where they start: a running stream's are
/// numbered from its `url`, a past broadcast's are listed below its
/// `fragment_base_url`. Anything else (an HLS manifest, a plain download)
/// has none.
fn fragments_of(f: &Value) -> Option<(String, Fragments)> {
    match f["protocol"].as_str()? {
        "http_dash_segments_generator" => {
            Some((f["url"].as_str()?.to_string(), Fragments::Numbered))
        }
        "http_dash_segments" => {
            let base = f["fragment_base_url"].as_str()?.to_string();
            let list = f["fragments"]
                .as_array()?
                .iter()
                .map(|p| Some((p["path"].as_str()?.to_string(), p["duration"].as_f64()?)))
                .collect::<Option<Vec<_>>>()
                .filter(|l| !l.is_empty())?;
            Some((base, Fragments::Listed(list)))
        }
        _ => None,
    }
}

/// Whether two tracks' fragments line up: both numbered, or the same paths.
fn same_cut(a: &Fragments, b: &Fragments) -> bool {
    match (a, b) {
        (Fragments::Numbered, Fragments::Numbered) => true,
        (Fragments::Listed(a), Fragments::Listed(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.0 == y.0)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: &str = include_str!("testdata/live-formats.json");

    fn live() -> Value {
        serde_json::from_str(LIVE).unwrap()
    }

    #[test]
    fn choose_takes_the_best_video_within_the_cap_and_the_best_audio() {
        let s = choose(&live(), 1440).unwrap();
        assert_eq!(
            s.video,
            Track {
                base: "https://gv.test/live/271".into(),
                ext: "mkv",
                format_id: "271".into()
            }
        );
        assert_eq!(
            s.audio,
            Track {
                base: "https://gv.test/live/140".into(),
                ext: "m4a",
                format_id: "140".into()
            }
        );
        assert_eq!(s.fragments, Fragments::Numbered);
        assert_eq!(s.target, 5.0);
        assert_eq!(s.title, "A live cam");
        assert_eq!(s.channel.as_deref(), Some("Cam"));
        assert_eq!(s.channel_id.as_deref(), Some("UCaaaaaaaaaaaaaaaaaaaaaa"));
        assert_eq!((s.started, s.watching), (Some(1771008448), Some(567)));
        assert!(!s.ended && s.rewind);
        // The cookie yt-dlp would send stays out.
        assert_eq!(s.headers, [("User-Agent".to_string(), "UA".to_string())]);

        let s = choose(&live(), 1080).unwrap();
        assert_eq!((s.video.format_id.as_str(), s.video.ext), ("248", "mkv"));
        assert_eq!(s.audio.format_id, "140");
    }

    #[test]
    fn an_mp4_video_is_named_mp4() {
        // Only H.264 up to 1080p: the last within the cap is 137.
        let mut v = live();
        v["formats"]
            .as_array_mut()
            .unwrap()
            .retain(|f| f["ext"] != "webm");
        let s = choose(&v, 4320).unwrap();
        assert_eq!((s.video.format_id.as_str(), s.video.ext), ("137", "mp4"));
    }

    #[test]
    fn choose_reads_a_past_broadcasts_fragment_list() {
        let v: Value = serde_json::from_str(
            r#"{"title":"Last night","live_status":"was_live","formats":[
              {"format_id":"140","ext":"m4a","protocol":"http_dash_segments","vcodec":"none","acodec":"mp4a.40.2",
               "url":"https://gv.test/manifest","fragment_base_url":"https://gv.test/a/",
               "fragments":[{"path":"sq/0","duration":5.0},{"path":"sq/1","duration":5.0},{"path":"sq/2","duration":2.5}]},
              {"format_id":"248","ext":"webm","protocol":"http_dash_segments","vcodec":"vp9","acodec":"none","height":1080,
               "url":"https://gv.test/manifest","fragment_base_url":"https://gv.test/v/",
               "fragments":[{"path":"sq/0","duration":5.0},{"path":"sq/1","duration":5.0},{"path":"sq/2","duration":2.5}]}
            ]}"#,
        )
        .unwrap();
        let s = choose(&v, 1080).unwrap();
        assert_eq!(
            s.fragments,
            Fragments::Listed(vec![
                ("sq/0".into(), 5.0),
                ("sq/1".into(), 5.0),
                ("sq/2".into(), 2.5)
            ])
        );
        assert_eq!(s.video.base, "https://gv.test/v/");
        assert_eq!(s.audio.base, "https://gv.test/a/");
        assert!(s.ended);
        // No target given: the longest fragment.
        assert_eq!(s.target, 5.0);
    }

    #[test]
    fn a_stream_without_fragments_is_refused() {
        // An HLS-only answer, or no formats at all.
        for json in [
            r#"{"live_status":"is_live","formats":[{"format_id":"95","protocol":"m3u8_native","vcodec":"avc1","acodec":"mp4a","height":720,"ext":"mp4","url":"https://gv.test/m"}]}"#,
            r#"{"live_status":"is_live","formats":[]}"#,
            r#"{"live_status":"is_live"}"#,
        ] {
            let e = choose(&serde_json::from_str(json).unwrap(), 1080).unwrap_err();
            assert_eq!(
                crate::errors::explain(&e.to_string()).code,
                crate::errors::Code::Live,
                "{json}"
            );
        }
        // Video without audio is no stream to play either.
        let mut v = live();
        v["formats"]
            .as_array_mut()
            .unwrap()
            .retain(|f| f["vcodec"] != "none");
        assert!(choose(&v, 1080).is_err());
        // Nor is a cap below every video.
        assert!(choose(&live(), 100).is_err());
    }
}
