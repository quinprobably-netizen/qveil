use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The settings file. A key this version doesn't know doesn't stop it from
/// loading (see `Config::load`): `save` writes every setting, so a file that a
/// newer qveil saved has keys that an older one has to get past.
#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
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
    /// Topic feeds: search by itself for what the channels you follow are
    /// about (see `topics`). Off unless it is turned on.
    pub topic_feeds: bool,
    /// Show thumbnails (fetched through the same route as everything else).
    pub thumbnails: bool,
    /// Near the end of a video played from a list, look up the next one in
    /// the list, so that it starts without waiting for yt-dlp. Off by
    /// default: it is a lookup of a video that may never be watched.
    pub prepare_next: bool,
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
            topic_feeds: false,
            thumbnails: true,
            prepare_next: false,
            feed_workers: 4,
            ytdlp: "yt-dlp".into(),
            mpv: "mpv".into(),
            mpv_args: vec![],
        }
    }
}

/// Every key a config file can have, as `save` writes them.
fn known_keys() -> Vec<String> {
    // `proxy` is left out of the file while it is unset.
    let all = Config {
        proxy: Some(String::new()),
        ..Config::default()
    };
    toml::Table::try_from(&all)
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

/// The keys of `text` that this version has no setting for, in order.
fn unknown_keys(text: &str) -> Vec<String> {
    let known = known_keys();
    let mut unknown: Vec<String> = text
        .parse::<toml::Table>()
        .map(|t| t.keys().filter(|k| !known.contains(*k)).cloned().collect())
        .unwrap_or_default();
    unknown.sort();
    unknown
}

impl Config {
    /// The settings in `path`, or the defaults if there is no file. Keys that
    /// this version doesn't know are left out and said so on stderr: a typo
    /// and a setting of a newer qveil look alike, and neither is a reason not
    /// to start. (`save` writes only the keys it knows.)
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let cfg =
                    toml::from_str(&s).with_context(|| format!("parsing {}", path.display()))?;
                let unknown = unknown_keys(&s);
                if !unknown.is_empty() {
                    note!(
                        "qveil: ignoring settings in {} that this version doesn't have: {} (from a newer qveil, or a typo)",
                        path.display(),
                        unknown.join(", ")
                    );
                }
                Ok(cfg)
            }
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
    use crate::store::testing::TempDir;

    const EXAMPLE: &str = include_str!("../config.example.toml");

    #[test]
    fn example_config_parses() {
        let c: Config = toml::from_str(EXAMPLE).unwrap();
        assert!(!c.history);
    }

    /// The example is where a new key is first documented, so it is held to
    /// what loading no longer enforces: no key it names is unknown, and every
    /// key there is has an entry (a setting whose default is "off" and which
    /// stays commented out, like `proxy`, counts).
    #[test]
    fn the_example_config_names_every_key_and_only_those() {
        assert_eq!(unknown_keys(EXAMPLE), Vec::<String>::new());
        let described = |key: &str| {
            EXAMPLE.lines().any(|line| {
                let line = line.trim_start_matches('#').trim_start();
                line.strip_prefix(key)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            })
        };
        let missing: Vec<_> = known_keys().into_iter().filter(|k| !described(k)).collect();
        assert!(missing.is_empty(), "config.example.toml lacks {missing:?}");
    }

    #[test]
    fn the_known_keys_are_the_ones_the_file_is_written_with() {
        let dir = TempDir::new();
        let path = dir.0.join("config.toml");
        Config {
            proxy: Some("socks5h://127.0.0.1:9050".into()),
            ..Config::default()
        }
        .save(&path)
        .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(unknown_keys(&written), Vec::<String>::new());
        let mut keys = known_keys();
        keys.sort();
        let mut written: Vec<_> = written
            .parse::<toml::Table>()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        written.sort();
        assert_eq!(keys, written);
        assert!(keys.contains(&"topic_feeds".to_string()), "{keys:?}");
    }

    #[test]
    fn a_config_with_a_key_this_version_does_not_know_still_loads() {
        // What a newer qveil wrote, or a typo: the settings that are known
        // count, and the rest is only reported.
        let dir = TempDir::new();
        let path = dir.0.join("config.toml");
        std::fs::write(
            &path,
            "max_height = 720\nhistory = true\nsomething_newer = \"x\"\nhistroy = true\n",
        )
        .unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.max_height, 720);
        assert!(c.history);
        assert_eq!(c.resume_days, 7, "what the file doesn't say stays default");
        assert_eq!(
            unknown_keys(&std::fs::read_to_string(&path).unwrap()),
            ["histroy", "something_newer"]
        );
        // A key that is known but wrongly typed is still an error.
        std::fs::write(&path, "max_height = \"tall\"\n").unwrap();
        assert!(Config::load(&path).is_err());
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
