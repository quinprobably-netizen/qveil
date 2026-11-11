//! What the tests of `app` share: runtimes and fake yt-dlps.

use super::runtime::Runtime;
use crate::config::Config;
use crate::store::Paths;
use crate::store::testing::TempDir;
use std::fs;

/// A runtime whose every request fails at once (nothing listens on the
/// proxy port) and whose yt-dlp is `ytdlp`.
pub(super) fn offline(paths: &Paths, ytdlp: &str) -> Runtime {
    let cfg = Config {
        ytdlp: ytdlp.into(),
        ..Config::default()
    };
    Runtime::new(cfg, paths, Some("http://127.0.0.1:1".into()), false).unwrap()
}

/// A yt-dlp that finds nothing through YouTube's own filter, and one
/// video for any other search; each search's target is added to `asked`.
pub(super) fn unfiltered_ytdlp(dir: &std::path::Path) -> String {
    crate::store::testing::script(
        &dir.join("yt-dlp"),
        r#"d=$(dirname "$0")
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && tail -n 1 "$2" >> "$d/asked"; shift; done
tail -n 1 "$d/asked" | grep -q "results?" && exit 0
n=$(wc -l < "$d/asked")
id=$(printf 'v%010d' "$n")
channel=$(printf 'UC%022d' "$n")
echo "{\"id\":\"$id\",\"title\":\"t\",\"channel\":\"C\",\"channel_id\":\"$channel\",\"duration\":2000.0}"
"#,
    )
}

pub(super) fn asked(dir: &TempDir) -> Vec<String> {
    fs::read_to_string(dir.0.join("asked"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

pub(super) fn channel(n: u32) -> String {
    format!("UC{n:022},http://x,Chan{n}\n")
}

pub(super) fn rt_cfg(ytdlp: &str) -> Config {
    Config {
        ytdlp: ytdlp.into(),
        ..Config::default()
    }
}

/// A runtime on Direct whose channel feeds come from `rss_base`, a local
/// stand-in for YouTube, and whose yt-dlp is `ytdlp`.
pub(super) fn with_feeds_at(paths: &Paths, rss_base: &str, ytdlp: &str) -> Runtime {
    let mut rt = Runtime::new(rt_cfg(ytdlp), paths, None, false).unwrap();
    rt.net = rt.net.allowing_http();
    rt.yt.net = rt.net.clone();
    rt.feed_base = rss_base.to_string();
    rt
}
