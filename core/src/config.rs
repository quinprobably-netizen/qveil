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
    /// Tor puts them on separate circuits.
    pub isolate_circuits: bool,
    /// Cap the video resolution.
    pub max_height: u32,
    /// Keep a local watch history. Off by default.
    pub history: bool,
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
            "# Written by veil when a setting changes in the app; comments are not kept.\n{}",
            toml::to_string_pretty(self)?
        );
        crate::store::write_atomic(path, text.as_bytes())
    }

    pub fn format(&self) -> String {
        format!("bv*[height<={h}]+ba/b[height<={h}]/b", h = self.max_height)
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
}
