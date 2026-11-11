use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The settings file. A key this version doesn't know doesn't stop it from
/// loading (see `Config::load`): `save` writes every setting, so a file that a
/// newer qveil saved has keys that an older one has to get past. One that is
/// a letter or two off a setting does stop it: that is a typo.
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
    /// Keep the thumbnails that were fetched on disk, in the cache directory,
    /// so that they aren't fetched again. Off by default: they are a record
    /// of what was shown.
    pub keep_thumbnails: bool,
    /// Near the end of a video played from a list, look up the next one in
    /// the list, so that it starts without waiting for yt-dlp. Off by
    /// default: it is a lookup of a video that may never be watched.
    pub prepare_next: bool,
    /// How many channel feeds to fetch at once.
    pub feed_workers: usize,
    /// Experimental, and off: look a video up without its watch page (see
    /// `ytdlp::LOOKUP`). Not in the window's settings.
    pub quick_lookup: bool,
    pub ytdlp: String,
    pub mpv: String,
    /// Extra arguments passed to mpv.
    pub mpv_args: Vec<String>,
    /// The keys of the file that this version has no setting for, and left
    /// out (see `load`). Not a setting: never written, and only ever filled
    /// in by `load`.
    #[serde(skip)]
    pub ignored: Vec<String>,
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
            keep_thumbnails: false,
            prepare_next: false,
            feed_workers: 4,
            quick_lookup: false,
            ytdlp: "yt-dlp".into(),
            mpv: "mpv".into(),
            mpv_args: vec![],
            ignored: vec![],
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

/// How far apart two keys are: the edits (a letter added, removed, changed,
/// or two neighbours swapped) that turn one into the other.
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    // d[i][j]: the distance between the first i letters of a and the first j of b.
    let mut d = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let change = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + change);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// How many edits apart an unknown key may be from a setting to be taken
/// for a typo of it.
const TYPO: usize = 2;

/// The setting that `key` (which isn't one) looks like a typo of, if any.
/// Case and `-` for `_` don't count as different.
fn meant(key: &str, known: &[String]) -> Option<String> {
    let typed = key.to_lowercase().replace('-', "_");
    known
        .iter()
        .map(|k| (distance(&typed, k), k))
        .filter(|(d, _)| *d <= TYPO)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k.clone())
}

impl Config {
    /// The settings in `path`, or the defaults if there is no file. A key
    /// that this version doesn't know is left out, said so on stderr and
    /// kept in `ignored` for the window to show: it may be a setting of a
    /// newer qveil, and that is no reason not to start. One that is within
    /// `TYPO` edits of a setting is refused instead: that is a typo, and a
    /// setting left out because of one can matter (`prxoy` would mean no
    /// proxy, and the real IP). (`save` writes only the keys it knows.)
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let mut cfg: Config =
                    toml::from_str(&s).with_context(|| format!("parsing {}", path.display()))?;
                let unknown = unknown_keys(&s);
                let known = known_keys();
                for key in &unknown {
                    if let Some(setting) = meant(key, &known) {
                        bail!(
                            "{}: `{key}` isn't a setting; did you mean `{setting}`? Correct it or remove it. qveil doesn't start with a setting that may be misspelt, because it would be left out (a misspelt `proxy` would mean no proxy)",
                            path.display()
                        );
                    }
                }
                if !unknown.is_empty() {
                    note!(
                        "qveil: ignoring settings in {} that this version doesn't have: {} (from a newer qveil?)",
                        path.display(),
                        unknown.join(", ")
                    );
                }
                cfg.ignored = unknown;
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
            "max_height = 720\nhistory = true\nsomething_newer = \"x\"\nchapters_on_the_bar = true\n",
        )
        .unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.max_height, 720);
        assert!(c.history);
        assert_eq!(c.resume_days, 7, "what the file doesn't say stays default");
        assert_eq!(
            unknown_keys(&std::fs::read_to_string(&path).unwrap()),
            ["chapters_on_the_bar", "something_newer"]
        );
        // And the window is told which, through `ignored`.
        assert_eq!(c.ignored, ["chapters_on_the_bar", "something_newer"]);
        assert!(Config::default().ignored.is_empty());
        // A key that is known but wrongly typed is still an error.
        std::fs::write(&path, "max_height = \"tall\"\n").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn a_key_a_letter_or_two_off_a_setting_stops_it_from_loading() {
        // `proxy` misspelt would otherwise be left out, and the engine would
        // start on Direct: the real IP, for someone who set up Tor by hand.
        let dir = TempDir::new();
        let path = dir.0.join("config.toml");
        for (typo, meant) in [
            ("prxoy", "proxy"),
            ("Proxy", "proxy"),
            ("proxi", "proxy"),
            ("PROXY", "proxy"),
            ("histroy", "history"),
            ("max-height", "max_height"),
            ("isolate_circuit", "isolate_circuits"),
        ] {
            std::fs::write(&path, format!("{typo} = \"socks5h://127.0.0.1:9050\"\n")).unwrap();
            let err = format!("{:#}", Config::load(&path).unwrap_err());
            assert!(err.contains(&format!("`{typo}`")), "{typo}: {err}");
            assert!(err.contains(&format!("`{meant}`")), "{typo}: {err}");
            assert!(err.contains(&path.display().to_string()), "{err}");
        }
        // One that is far from every setting is a newer qveil's, and passes.
        std::fs::write(&path, "proxy_rotation_minutes = 5\n").unwrap();
        assert_eq!(
            Config::load(&path).unwrap().ignored,
            ["proxy_rotation_minutes"]
        );
    }

    #[test]
    fn the_distance_counts_edits_and_swaps() {
        assert_eq!(distance("proxy", "proxy"), 0);
        assert_eq!(distance("prxoy", "proxy"), 1);
        assert_eq!(distance("proxi", "proxy"), 1);
        assert_eq!(distance("prox", "proxy"), 1);
        assert_eq!(distance("pproxyy", "proxy"), 2);
        assert_eq!(distance("mpv", "proxy"), 5);
        assert_eq!(distance("", "abc"), 3);
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
