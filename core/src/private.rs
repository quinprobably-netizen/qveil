//! Scratch directories for handing secrets to a child process.
//!
//! Anything in a command line is readable by every local user through
//! /proc/*/cmdline, so what must stay private (the URLs mpv plays, what yt-dlp
//! is asked for, the proxy's password) goes through files in a directory that
//! only this user can open, and the child is given just the paths.

use anyhow::{Context, Result, bail};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Once;

/// Name prefixes of the directories made here: `qveil-<what>-<pid>-<random>`.
const PREFIXES: &[&str] = &["qveil-mpv-", "qveil-ytdlp-"];

/// A fresh directory (mode 0700), removed with everything in it when dropped.
pub struct PrivateDir(PathBuf);

impl PrivateDir {
    /// `prefix` is one of `PREFIXES`, which is how a later run knows the
    /// directory is ours when it sweeps up after a killed process.
    pub fn new(prefix: &str) -> Result<Self> {
        debug_assert!(PREFIXES.contains(&prefix));
        // Prefers `$XDG_RUNTIME_DIR`: per-user, mode 0700 and on tmpfs, so
        // nothing reaches disk. Falls back to the system temp dir.
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        static SWEEP: Once = Once::new();
        SWEEP.call_once(|| sweep_stale(&base));
        Self::create_in(&base, prefix)
    }

    fn create_in(base: &Path, prefix: &str) -> Result<Self> {
        for _ in 0..8 {
            let name = format!(
                "{prefix}{}-{:016x}",
                std::process::id(),
                rand::random::<u64>()
            );
            let dir = base.join(name);
            // Not recursive: it must fail if the name is taken, so the directory is ours alone.
            match DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
            }
        }
        bail!(
            "couldn't create a private directory under {}",
            base.display()
        )
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Writes a new file (mode 0600) and returns its path.
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.0.join(name);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut f| f.write_all(bytes))
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Ctrl+C takes qveil down along with its child, and a process that is killed
/// can't clean up after itself. So a run removes the directories of runs whose
/// process is gone, which keeps a title or a search from lingering in the temp dir.
fn sweep_stale(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name
            .to_str()
            .and_then(|n| PREFIXES.iter().find_map(|p| n.strip_prefix(p)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempDir;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_directory_and_its_files_are_private_and_removed_afterwards() {
        let base = TempDir::new();
        let dir = PrivateDir::create_in(&base.0, "qveil-ytdlp-").unwrap();
        let file = dir.write("secret", b"x").unwrap();
        assert_eq!((mode(dir.path()), mode(&file)), (0o700, 0o600));
        assert!(
            dir.write("secret", b"y").is_err(),
            "an existing file is never reopened"
        );
        let path = dir.path().to_path_buf();
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn directories_of_dead_runs_are_swept() {
        let base = TempDir::new();
        let mine = format!("-{}-0123456789abcdef", std::process::id());
        let (dead_mpv, dead_ytdlp, live, other) = (
            base.0.join("qveil-mpv-999999999-0123456789abcdef"),
            base.0.join("qveil-ytdlp-999999999-0123456789abcdef"),
            base.0.join(format!("qveil-ytdlp{mine}")),
            base.0.join("unrelated"),
        );
        for d in [&dead_mpv, &dead_ytdlp, &live, &other] {
            fs::create_dir(d).unwrap();
            fs::write(d.join("options.conf"), "x").unwrap();
        }
        sweep_stale(&base.0);
        assert!(
            !dead_mpv.exists() && !dead_ytdlp.exists(),
            "a run whose process is gone leaves nothing"
        );
        assert!(live.exists(), "a live run keeps its files");
        assert!(other.exists(), "only qveil's own directories are touched");
    }
}
