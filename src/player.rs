//! Playback through mpv with no browser, no cookies and no player state.
//!
//! Stream URLs are tied to the IP that resolved them, so the bytes must
//! come through the same proxy as the metadata:
//! * no proxy / HTTP proxy: veil resolves streams with yt-dlp and hands mpv
//!   the direct URLs plus `--http-proxy`. Seeking works.
//! * SOCKS (Tor): mpv's HTTP stack can't use SOCKS, so yt-dlp downloads
//!   through the proxy and pipes into mpv. Seeking is limited, but nothing
//!   bypasses the proxy.

use crate::config::Config;
use crate::ids::VideoRef;
use crate::net::Net;
use crate::ytdlp::Ytdlp;
use anyhow::{Context, Result, bail};
use std::process::{Command, Stdio};

fn mpv_base(cfg: &Config, title: &str) -> Command {
    let mut c = Command::new(&cfg.mpv);
    c.args([
        "--no-ytdl",                  // veil already resolved the streams; mpv must not re-fetch
        "--save-position-on-quit=no", // no watch_later files naming what you watched
        "--save-watch-history=no",
        "--force-window=immediate",
    ]);
    c.arg(format!("--force-media-title={title}"));
    c.args(&cfg.mpv_args);
    c
}

/// Plays the video and returns its title.
pub fn play(
    cfg: &Config,
    net: &Net,
    yt: &Ytdlp,
    v: &VideoRef,
    title_hint: Option<&str>,
) -> Result<String> {
    let url = v.watch_url();
    if net.is_socks() {
        let title = title_hint.unwrap_or(&v.id).to_string();
        let mut dl = yt.cmd();
        dl.args([
            "-q",
            "--no-playlist",
            "-f",
            &cfg.format(),
            "-o",
            "-",
            "--",
            &url,
        ])
        .stdout(Stdio::piped());
        let mut dl = dl
            .spawn()
            .with_context(|| format!("running {}", cfg.ytdlp))?;
        let pipe = dl.stdout.take().context("yt-dlp stdout")?;
        eprintln!("streaming through the SOCKS proxy (seeking is limited)…");
        let status = mpv_base(cfg, &title)
            .arg("-")
            .stdin(Stdio::from(pipe))
            .status()
            .with_context(|| format!("running {}", cfg.mpv));
        let _ = dl.kill();
        let _ = dl.wait();
        check(status?)?;
        return Ok(title);
    }

    let s = yt.resolve(&url, &cfg.format())?;
    let mut mpv = mpv_base(cfg, &s.title);
    if let Some(p) = net.proxy_url() {
        mpv.arg(format!("--http-proxy={p}"));
    }
    for (k, val) in &s.headers {
        if k.eq_ignore_ascii_case("user-agent") {
            mpv.arg(format!("--user-agent={val}"));
        } else {
            // -append adds one item without splitting on commas
            mpv.arg(format!("--http-header-fields-append={k}: {val}"));
        }
    }
    if let Some(start) = v.start {
        mpv.arg(format!("--start={start}"));
    }
    if let Some(a) = &s.audio {
        mpv.arg(format!("--audio-file={a}"));
    }
    mpv.arg("--").arg(&s.video).stdin(Stdio::null());
    check(
        mpv.status()
            .with_context(|| format!("running {}", cfg.mpv))?,
    )?;
    Ok(s.title)
}

fn check(status: std::process::ExitStatus) -> Result<()> {
    // 4 = interrupted by a signal (Ctrl+C); that's a user quit, not a failure
    match status.code() {
        Some(0) | Some(4) => Ok(()),
        _ => bail!("mpv exited with {status}"),
    }
}
