//! yt-dlp does the work that breaks every few weeks (InnerTube parsing,
//! signature/n-param deciphering, PO tokens), so veil follows its fixes for free.
//! Every invocation is sandboxed from the user's own yt-dlp setup.

use crate::errors::friendly;
use crate::ids;
use crate::net::Net;
use crate::video::Video;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::fmt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Why a live stream is refused: the relay can only carry a plain download.
const LIVE_UNSUPPORTED: &str =
    "Live streams aren't supported yet: they can't be relayed through your route.";
const MANIFEST_ONLY: &str = "This video is only offered as an HLS/DASH stream, which can't be relayed through your route yet.";

/// yt-dlp exited with an error. `raw` is its last stderr line; the message
/// shown is the readable version of it.
#[derive(Debug)]
struct Failed {
    raw: String,
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&friendly(&format!("yt-dlp: {}", self.raw)))
    }
}

impl std::error::Error for Failed {}

pub struct Ytdlp {
    pub bin: String,
    pub cache_dir: PathBuf,
    pub net: Net,
}

#[derive(Debug)]
pub struct Streams {
    pub title: String,
    pub channel: Option<String>,
    pub channel_id: Option<String>,
    pub duration: Option<u64>,
    pub video: String,
    pub audio: Option<String>,
    pub headers: Vec<(String, String)>,
}

impl Ytdlp {
    pub fn cmd(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.args([
            "--ignore-config", // a user config could add cookies or a browser profile
            "--no-cookies",
            "--no-cookies-from-browser",
            "--no-mark-watched",
            "--no-warnings",
        ]);
        c.arg("--cache-dir").arg(&self.cache_dir);
        if let Some(p) = self.net.proxy_url() {
            c.arg("--proxy").arg(p);
        }
        c.stdin(Stdio::null());
        c
    }

    fn json(&self, args: &[&str]) -> Result<Value> {
        let out = self
            .cmd()
            .args(args)
            .output()
            .with_context(|| format!("running {:?} (is yt-dlp installed?)", self.bin))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let line = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("unknown error")
                .trim();
            return Err(Failed {
                raw: line.strip_prefix("ERROR: ").unwrap_or(line).into(),
            }
            .into());
        }
        serde_json::from_slice(&out.stdout).context("yt-dlp returned invalid JSON")
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Video>> {
        let q = format!("ytsearch{limit}:{query}");
        let v = self.json(&["--flat-playlist", "-J", "--", &q])?;
        Ok(entries(&v))
    }

    /// Returns (channel id, display name).
    pub fn channel_info(&self, url: &str) -> Result<(String, String)> {
        let v = self.json(&["--flat-playlist", "-J", "-I", "0", "--", url])?;
        let id = str_field(&v, &["channel_id", "id"])
            .filter(|id| ids::is_channel_id(id))
            .with_context(|| format!("{url} doesn't look like a channel"))?;
        let name = str_field(&v, &["channel", "uploader", "title"]).unwrap_or_else(|| id.clone());
        Ok((id, name))
    }

    /// Latest uploads from the channel's Videos tab. Used as a fallback when
    /// the RSS feed is down. `published` is only approximate (day-level,
    /// parsed from "3 days ago").
    pub fn channel_videos(&self, channel_id: &str, limit: usize) -> Result<Vec<Video>> {
        let url = format!("https://www.youtube.com/channel/{channel_id}/videos");
        let range = format!("1:{limit}");
        let v = self.json(&[
            "--flat-playlist",
            "--extractor-args",
            "youtubetab:approximate_date",
            "-J",
            "-I",
            &range,
            "--",
            &url,
        ])?;
        Ok(entries(&v))
    }

    /// Picks the streams to relay. `format` must only select plain downloads
    /// (see `Config::format`), and the result is checked again here because a
    /// manifest slipping through would make mpv fetch segments directly,
    /// around the route.
    pub fn resolve(&self, url: &str, format: &str) -> Result<Streams> {
        match self.json(&["-J", "--no-playlist", "-f", format, "--", url]) {
            Ok(v) => streams_from(&v, url),
            Err(e) if is_no_format(&e) => Err(self.explain_no_format(url, e)),
            Err(e) => Err(e),
        }
    }

    /// The format selector only accepts plain downloads, so a live stream
    /// (HLS only) makes yt-dlp fail with a bare "no format". Ask again with
    /// its default selector, which does return something, to say why. This
    /// costs a second request, but only on a path that was failing anyway.
    fn explain_no_format(&self, url: &str, original: anyhow::Error) -> anyhow::Error {
        match self.json(&["-J", "--no-playlist", "--", url]) {
            Ok(v) if is_live(&v) => anyhow!(LIVE_UNSUPPORTED),
            Ok(v) if !chosen_formats(&v).iter().all(|f| is_plain(f)) => anyhow!(MANIFEST_ONLY),
            _ => original,
        }
    }
}

fn is_no_format(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Failed>()
        .is_some_and(|f| f.raw.contains("Requested format is not available"))
}

fn is_live(v: &Value) -> bool {
    v["is_live"].as_bool() == Some(true) || v["live_status"].as_str() == Some("is_live")
}

/// The formats yt-dlp chose: both halves of a merge, or the video itself.
fn chosen_formats(v: &Value) -> Vec<&Value> {
    match v["requested_formats"].as_array() {
        Some(fs) => fs.iter().collect(),
        None => vec![v],
    }
}

/// A plain download, as opposed to an HLS or DASH manifest. `http_dash_segments`
/// also starts with "http", so this compares the whole protocol. The selector
/// asks for https only because the relay's clients refuse plain http.
fn is_plain(f: &Value) -> bool {
    matches!(f["protocol"].as_str(), Some("https" | "http"))
}

fn streams_from(v: &Value, url: &str) -> Result<Streams> {
    if is_live(v) {
        bail!(LIVE_UNSUPPORTED);
    }
    let title = str_field(v, &["title"]).unwrap_or_else(|| url.into());
    let formats = chosen_formats(v);
    if !formats.iter().all(|f| is_plain(f)) {
        bail!(MANIFEST_ONLY);
    }
    let is_video = |f: &&Value| f["vcodec"].as_str() != Some("none");
    let video_fmt = formats
        .iter()
        .copied()
        .find(is_video)
        .or(formats.first().copied())
        .context("no playable format")?;
    let audio = formats
        .iter()
        .find(|f| !is_video(f))
        .and_then(|f| f["url"].as_str())
        .map(String::from);
    let headers = video_fmt["http_headers"]
        .as_object()
        .map(|h| {
            h.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Ok(Streams {
        title,
        channel: str_field(v, &["channel", "uploader"]),
        channel_id: str_field(v, &["channel_id"]).filter(|c| ids::is_channel_id(c)),
        duration: v["duration"].as_f64().map(|d| d as u64),
        video: video_fmt["url"]
            .as_str()
            .context("format has no URL")?
            .into(),
        audio,
        headers,
    })
}

fn str_field(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| v[*k].as_str().filter(|s| !s.is_empty()).map(String::from))
}

fn entries(v: &Value) -> Vec<Video> {
    let parent_channel = str_field(v, &["channel", "uploader"]);
    let parent_id = str_field(v, &["channel_id"]);
    v["entries"]
        .as_array()
        .map(|es| {
            es.iter()
                .filter_map(|e| entry(e, &parent_channel, &parent_id))
                .collect()
        })
        .unwrap_or_default()
}

fn entry(e: &Value, channel: &Option<String>, channel_id: &Option<String>) -> Option<Video> {
    let id = e["id"].as_str().filter(|id| ids::is_video_id(id))?;
    Some(Video {
        id: id.into(),
        title: str_field(e, &["title"]).unwrap_or_else(|| id.into()),
        channel: str_field(e, &["channel", "uploader"]).or_else(|| channel.clone()),
        channel_id: str_field(e, &["channel_id"]).or_else(|| channel_id.clone()),
        published: e["timestamp"].as_i64().or(e["release_timestamp"].as_i64()),
        duration: e["duration"].as_f64().map(|d| d as u64),
        views: e["view_count"].as_u64(),
        short: e["url"].as_str().is_some_and(|u| u.contains("/shorts/")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(json: &str) -> Result<Streams> {
        streams_from(
            &serde_json::from_str(json).unwrap(),
            "https://example.test/v",
        )
    }

    #[test]
    fn plain_downloads_resolve() {
        let s = resolved(
            r#"{"title":"T","duration":12.5,"protocol":"https+https","requested_formats":[
                {"url":"https://gv.test/v","protocol":"https","vcodec":"av01","http_headers":{"User-Agent":"x"}},
                {"url":"https://gv.test/a","protocol":"https","vcodec":"none"}]}"#,
        )
        .unwrap();
        assert_eq!(
            (s.video.as_str(), s.audio.as_deref()),
            ("https://gv.test/v", Some("https://gv.test/a"))
        );
        assert_eq!(s.headers, [("User-Agent".to_string(), "x".to_string())]);
        // A single combined format has its protocol at the top level.
        let s = resolved(
            r#"{"title":"T","url":"https://gv.test/c","protocol":"https","vcodec":"avc1"}"#,
        )
        .unwrap();
        assert_eq!((s.video.as_str(), s.audio), ("https://gv.test/c", None));
    }

    #[test]
    fn live_streams_are_refused() {
        for json in [
            r#"{"is_live":true,"url":"https://gv.test/x","protocol":"https"}"#,
            r#"{"live_status":"is_live","url":"https://gv.test/x","protocol":"https"}"#,
        ] {
            let e = resolved(json).unwrap_err().to_string();
            assert_eq!(e, LIVE_UNSUPPORTED);
        }
        // A finished stream (`was_live`) is an ordinary download.
        assert!(
            resolved(r#"{"live_status":"was_live","url":"https://gv.test/x","protocol":"https"}"#)
                .is_ok()
        );
    }

    #[test]
    fn manifests_are_refused_even_when_one_half_is_plain() {
        for protocol in ["m3u8_native", "m3u8", "http_dash_segments", "mhtml", "rtmp"] {
            let json = format!(
                r#"{{"requested_formats":[
                    {{"url":"https://gv.test/v","protocol":"https","vcodec":"avc1"}},
                    {{"url":"https://gv.test/a","protocol":"{protocol}","vcodec":"none"}}]}}"#
            );
            assert_eq!(
                resolved(&json).unwrap_err().to_string(),
                MANIFEST_ONLY,
                "{protocol}"
            );
        }
        assert!(
            resolved(r#"{"url":"https://gv.test/x"}"#).is_err(),
            "no protocol, no relay"
        );
    }

    #[test]
    fn parses_flat_entries() {
        let v: Value = serde_json::from_str(
            r#"{"channel":"Chan","channel_id":"UCsBjURrPoezykLs9EqgamOA","entries":[
                {"id":"5C_HPTJg5ek","title":"Rust in 100 Seconds","duration":149.0,"view_count":2648797},
                {"id":"UCxxxxxxxxxxxxxxxxxxxxxx","title":"a channel result"},
                {"id":"abcdefghijk","url":"https://www.youtube.com/shorts/abcdefghijk","channel":"Other"}
            ]}"#,
        )
        .unwrap();
        let es = entries(&v);
        assert_eq!(es.len(), 2);
        assert_eq!(es[0].channel.as_deref(), Some("Chan"));
        assert_eq!(es[0].duration, Some(149));
        assert!(es[1].short);
        assert_eq!(es[1].channel.as_deref(), Some("Other"));
    }
}
