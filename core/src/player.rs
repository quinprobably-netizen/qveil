//! Command-line playback in a separate mpv window. mpv only ever connects to
//! veil's loopback relay, so it needs no proxy, cookies or headers, and it
//! keeps no player state.

use crate::config::Config;
use crate::server::PlayInfo;
use anyhow::{Context, Result, bail};
use std::process::{Command, Stdio};

pub fn play(cfg: &Config, info: &PlayInfo) -> Result<()> {
    let mut c = Command::new(&cfg.mpv);
    c.args([
        "--no-ytdl",
        "--save-position-on-quit=no", // no watch_later files naming what you watched
        "--save-watch-history=no",
        "--force-window=immediate",
    ]);
    c.arg(format!("--force-media-title={}", info.title));
    if let Some(start) = info.start {
        c.arg(format!("--start={start}"));
    }
    if let Some(a) = &info.audio {
        c.arg(format!("--audio-file={a}"));
    }
    c.args(&cfg.mpv_args);
    c.arg("--").arg(&info.video).stdin(Stdio::null());
    // A proxy from the environment must not catch loopback requests.
    for var in [
        "http_proxy",
        "HTTP_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
    ] {
        c.env_remove(var);
    }
    let status = c.status().with_context(|| format!("running {}", cfg.mpv))?;
    // 4 = interrupted by a signal (Ctrl+C); that's a user quit, not a failure.
    match status.code() {
        Some(0) | Some(4) => Ok(()),
        _ => bail!("mpv exited with {status}"),
    }
}
