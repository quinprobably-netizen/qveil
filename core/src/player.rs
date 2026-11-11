//! Command-line playback in a separate mpv window. mpv only ever connects to
//! qveil's loopback relay, so it needs no proxy, cookies or headers, and it
//! keeps no player state.
//!
//! The relay URLs carry the server token and the title says what you watch,
//! and anything in argv is readable by every local user through
//! /proc/*/cmdline. So they go to mpv through files in a private directory
//! instead, which is removed once mpv exits.

use crate::config::Config;
use crate::private::PrivateDir;
use crate::server::PlayInfo;
use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub fn play(cfg: &Config, info: &PlayInfo) -> Result<()> {
    let staged = Staged::new(info)?;
    let mut c = Command::new(&cfg.mpv);
    c.args(mpv_args(&staged));
    c.args(&cfg.mpv_args).stdin(Stdio::null());
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
    // Through the engine's one way of starting a child, so that mpv ends with
    // the engine, however that ends, and so does whatever mpv starts.
    let status = crate::proc::spawn(&mut c)
        .and_then(|mut mpv| mpv.wait())
        .with_context(|| format!("running {}", cfg.mpv))?;
    // 4 = interrupted by a signal (Ctrl+C); that's a user quit, not a failure.
    match status.code() {
        Some(0) | Some(4) => Ok(()),
        _ => bail!("mpv exited with {status}"),
    }
}

/// Everything mpv is told except the secrets, which are in `staged`'s files.
fn mpv_args(staged: &Staged) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        // The same isolation as the embedded player in src/mpvitem.cpp: nothing
        // from the user's mpv setup, whose scripts (ytdl_hook among them) could
        // contact YouTube directly.
        "--config=no",
        "--load-scripts=no",
        "--load-stats-overlay=no",
        "--load-console=no",
        "--load-osd-console=no",
        "--load-select=no",
        "--load-commands=no",
        "--load-context-menu=no",
        "--load-positioning=no",
        "--load-auto-profiles=no",
        "--no-ytdl",
        "--save-position-on-quit=no", // no watch_later files naming what you watched
        "--save-watch-history=no",
        "--force-window=immediate",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    // `--include` still loads with `--config=no`: only the default files are skipped.
    for (flag, path) in [
        ("--include=", &staged.options),
        ("--playlist=", &staged.playlist),
    ] {
        let mut arg = OsString::from(flag);
        arg.push(path);
        args.push(arg);
    }
    args
}

/// A private directory with the video URL as a playlist and the remaining
/// options as a config file, removed when this is dropped.
struct Staged {
    _dir: PrivateDir,
    playlist: PathBuf,
    options: PathBuf,
}

impl Staged {
    fn new(info: &PlayInfo) -> Result<Self> {
        let dir = PrivateDir::new("qveil-mpv-")?;
        let playlist = dir.write("video.m3u", format!("{}\n", info.video).as_bytes())?;
        let options = dir.write("options.conf", options_file(info).as_bytes())?;
        Ok(Staged {
            _dir: dir,
            playlist,
            options,
        })
    }
}

fn options_file(info: &PlayInfo) -> String {
    let mut out = String::new();
    if let Some(a) = &info.audio {
        out.push_str(&format!("audio-file={}\n", quote(a)));
    }
    let title = single_line(&info.title);
    if !title.is_empty() {
        out.push_str(&format!("force-media-title={}\n", quote(&title)));
    }
    if let Some(start) = info.start {
        out.push_str(&format!("start={start}\n"));
    }
    out
}

/// mpv's config syntax: `%n%` followed by exactly n bytes is taken verbatim,
/// so `#`, quotes and `%` in a title can't be misread as comments or syntax.
fn quote(value: &str) -> String {
    format!("%{}%{value}", value.len())
}

/// Config files are read line by line, so a newline in a title would end the value early.
fn single_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn info() -> PlayInfo {
        PlayInfo {
            id: "dQw4w9WgXcQ".into(),
            title: "A #1 \"hit\"\nsecond line".into(),
            channel: None,
            channel_id: None,
            duration: None,
            start: Some(90),
            resume: None,
            video: "http://127.0.0.1:4000/SECRETTOKEN/s/sid/v".into(),
            audio: Some("http://127.0.0.1:4000/SECRETTOKEN/s/sid/a".into()),
            description: None,
            views: None,
            likes: None,
            published: None,
            chapters: vec![],
            chapters_later: false,
            details_later: false,
        }
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn secrets_go_through_private_files_not_argv() {
        let staged = Staged::new(&info()).unwrap();
        assert_eq!(mode(staged._dir.path()), 0o700);
        assert_eq!(mode(&staged.playlist), 0o600);
        assert_eq!(mode(&staged.options), 0o600);

        let argv = mpv_args(&staged)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for arg in &argv {
            assert!(!arg.contains("SECRETTOKEN"), "token in argv: {arg}");
            assert!(!arg.contains("hit"), "title in argv: {arg}");
        }
        for flag in [
            "--config=no",
            "--load-scripts=no",
            "--no-ytdl",
            "--save-position-on-quit=no",
            "--save-watch-history=no",
        ] {
            assert!(argv.iter().any(|a| a == flag), "missing {flag}");
        }
        assert!(argv.contains(&format!("--playlist={}", staged.playlist.display())));
        assert!(argv.contains(&format!("--include={}", staged.options.display())));

        assert_eq!(
            fs::read_to_string(&staged.playlist).unwrap(),
            "http://127.0.0.1:4000/SECRETTOKEN/s/sid/v\n"
        );
        let options = fs::read_to_string(&staged.options).unwrap();
        // The title is 22 bytes once its newline is flattened to a space.
        assert_eq!(
            options,
            "audio-file=%41%http://127.0.0.1:4000/SECRETTOKEN/s/sid/a\n\
             force-media-title=%22%A #1 \"hit\" second line\n\
             start=90\n"
        );
    }

    #[test]
    fn files_are_removed_afterwards() {
        let staged = Staged::new(&info()).unwrap();
        let dir = staged._dir.path().to_path_buf();
        assert!(dir.is_dir());
        drop(staged);
        assert!(!dir.exists());
    }

    #[test]
    fn options_are_left_out_when_absent() {
        let mut i = info();
        (i.audio, i.start, i.title) = (None, None, "  ".into());
        assert_eq!(options_file(&i), "");
    }
}
