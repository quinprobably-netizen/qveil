//! yt-dlp does the work that breaks every few weeks (InnerTube parsing,
//! signature/n-param deciphering, PO tokens), so veil follows its fixes for free.
//! Every invocation is sandboxed from the user's own yt-dlp setup.

use crate::ids;
use crate::net::Net;
use crate::video::Video;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub struct Ytdlp {
    pub bin: String,
    pub cache_dir: PathBuf,
    pub net: Net,
}

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
                .unwrap_or("unknown error");
            bail!("yt-dlp: {}", line.trim());
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

    pub fn resolve(&self, url: &str, format: &str) -> Result<Streams> {
        let v = self.json(&["-J", "--no-playlist", "-f", format, "--", url])?;
        let title = str_field(&v, &["title"]).unwrap_or_else(|| url.into());
        let formats: Vec<&Value> = match v["requested_formats"].as_array() {
            Some(fs) => fs.iter().collect(),
            None => vec![&v],
        };
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
            channel: str_field(&v, &["channel", "uploader"]),
            channel_id: str_field(&v, &["channel_id"]).filter(|c| ids::is_channel_id(c)),
            duration: v["duration"].as_f64().map(|d| d as u64),
            video: video_fmt["url"]
                .as_str()
                .context("format has no URL")?
                .into(),
            audio,
            headers,
        })
    }
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
