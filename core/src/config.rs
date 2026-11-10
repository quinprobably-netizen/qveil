use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Route every request (search, feeds, metadata *and* video bytes)
    /// through this proxy: `http://…`, `https://…` or `socks5h://…`.
    pub proxy: Option<String>,
    /// With a SOCKS proxy, use new random credentials for each request so
    /// Tor puts them on separate circuits. Not optional for any SOCKS proxy
    /// without credentials of its own (`false` there is refused, see
    /// `Net::new`), because a proxy can't be told from Tor by its address.
    /// The key is still accepted so that existing config files load.
    pub isolate_circuits: bool,
    /// Cap the video resolution.
    pub max_height: u32,
    /// Keep a local watch history. Off by default.
    pub history: bool,
    /// For how many days qveil remembers where an unfinished video was
    /// stopped. 0 turns that off, and deletes what was remembered.
    pub resume_days: u32,
    /// Show thumbnails (fetched through the same route as everything else).
    pub thumbnails: bool,
    /// How many channel feeds to fetch at once.
    pub feed_workers: usize,
    pub ytdlp: String,
    pub mpv: String,
    /// Extra arguments passed to mpv.
    pub mpv_args: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            proxy: None,
            isolate_circuits: true,
            max_height: 1080,
            history: false,
            resume_days: 7,
            thumbnails: true,
            feed_workers: 4,
            ytdlp: "yt-dlp".into(),
            mpv: "mpv".into(),
            mpv_args: vec![],
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = format!(
            "# Written by qveil when a setting changes in the app; comments are not kept.\n{}",
            toml::to_string_pretty(self)?
        );
        crate::store::write_atomic(path, text.as_bytes())
    }

    /// The yt-dlp format selector. Every branch is limited to plain https
    /// downloads: the relay forwards one URL per stream, so an HLS or DASH
    /// manifest (live streams, some formats) would hand mpv segment URLs that
    /// it fetches directly, around the route. `protocol=https` is compared in
    /// full because `protocol^=http` also matches `http_dash_segments`.
    pub fn format(&self) -> String {
        let (h, p) = (self.max_height, "[protocol=https]");
        format!("bv*[height<={h}]{p}+ba{p}/b[height<={h}]{p}/b{p}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let c: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        assert!(!c.history);
        assert!(
            toml::from_str::<Config>("histroy = true").is_err(),
            "typos must not be silently ignored"
        );
    }

    #[test]
    fn resume_is_seven_days_unless_the_file_says_otherwise() {
        assert_eq!(Config::default().resume_days, 7);
        assert_eq!(
            toml::from_str::<Config>("resume_days = 3")
                .unwrap()
                .resume_days,
            3
        );
        // A config written before the key existed.
        assert_eq!(
            toml::from_str::<Config>("history = true")
                .unwrap()
                .resume_days,
            7
        );
    }

    #[test]
    fn every_format_branch_is_a_plain_download() {
        let format = Config::default().format();
        assert_eq!(
            format,
            "bv*[height<=1080][protocol=https]+ba[protocol=https]/b[height<=1080][protocol=https]/b[protocol=https]"
        );
        for part in format.split(['/', '+']) {
            assert!(part.contains("[protocol=https]"), "{part} is unfiltered");
        }
    }
}
