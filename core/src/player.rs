//! Command-line playback in a separate mpv window. mpv only ever connects to
//! qveil's loopback relay, so it needs no proxy, cookies or headers, and it
//! keeps no player state.
//!
//! The relay URLs carry the server token and the title says what you watch,
//! and anything in argv is readable by every local user through
//! /proc/*/cmdline. So they go to mpv through files in a private directory
//! instead, which is removed once mpv exits.

use crate::config::Config;
use crate::server::PlayInfo;
use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
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
    let status = c.status().with_context(|| format!("running {}", cfg.mpv))?;
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

/// A fresh directory (mode 0700) with the video URL as a playlist and the
/// remaining options as a config file (both mode 0600).
struct Staged {
    dir: PathBuf,
    playlist: PathBuf,
    options: PathBuf,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Staged {
    fn new(info: &PlayInfo) -> Result<Self> {
        let dir = private_dir()?;
        // From here on, `Drop` removes the directory if a write fails.
        let staged = Staged {
            playlist: dir.join("video.m3u"),
            options: dir.join("options.conf"),
            dir,
        };
        write_private(&staged.playlist, format!("{}\n", info.video).as_bytes())?;
        write_private(&staged.options, options_file(info).as_bytes())?;
        Ok(staged)
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

/// Prefers `$XDG_RUNTIME_DIR`: per-user, mode 0700 and on tmpfs, so nothing
/// reaches disk. Falls back to the system temp dir.
fn private_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    sweep_stale(&base);
    for _ in 0..8 {
        let name = format!(
            "qveil-mpv-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        );
        let dir = base.join(name);
        // Not recursive: it must fail if the name is taken, so the directory is ours alone.
        match DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
        }
    }
    bail!(
        "couldn't create a private directory under {}",
        base.display()
    )
}

/// Ctrl+C takes qveil down along with mpv, and a process that is killed can't
/// clean up after itself. So each run removes the directories of runs whose
/// process is gone, which keeps a title from lingering in the temp dir.
fn sweep_stale(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name
            .to_str()
            .and_then(|n| n.strip_prefix("qveil-mpv-"))
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok());
        if let Some(pid) = pid
            && !Path::new("/proc").join(pid.to_string()).exists()
        {
            // Doesn't follow symlinks; one with this name would just be unlinked.
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn info() -> PlayInfo {
        PlayInfo {
            id: "dQw4w9WgXcQ".into(),
            title: "A #1 \"hit\"\nsecond line".into(),
            channel: None,
            channel_id: None,
            duration: None,
            start: Some(90),
            video: "http://127.0.0.1:4000/SECRETTOKEN/s/sid/v".into(),
            audio: Some("http://127.0.0.1:4000/SECRETTOKEN/s/sid/a".into()),
        }
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn secrets_go_through_private_files_not_argv() {
        let staged = Staged::new(&info()).unwrap();
        assert_eq!(mode(&staged.dir), 0o700);
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
        let dir = staged.dir.clone();
        assert!(dir.is_dir());
        drop(staged);
        assert!(!dir.exists());
    }

    #[test]
    fn directories_of_dead_runs_are_swept() {
        let base = crate::store::testing::TempDir::new();
        let (dead, mine, other) = (
            base.0.join("qveil-mpv-999999999-0123456789abcdef"),
            base.0
                .join(format!("qveil-mpv-{}-0123456789abcdef", std::process::id())),
            base.0.join("unrelated"),
        );
        for d in [&dead, &mine, &other] {
            fs::create_dir(d).unwrap();
            fs::write(d.join("options.conf"), "x").unwrap();
        }
        sweep_stale(&base.0);
        assert!(!dead.exists(), "a run whose process is gone leaves nothing");
        assert!(mine.exists(), "a live run keeps its files");
        assert!(other.exists(), "only qveil's own directories are touched");
    }

    #[test]
    fn options_are_left_out_when_absent() {
        let mut i = info();
        (i.audio, i.start, i.title) = (None, None, "  ".into());
        assert_eq!(options_file(&i), "");
    }
}
