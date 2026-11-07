//! Local state. Everything lives under the XDG dirs with owner-only
//! permissions, and nothing is ever sent anywhere.

use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        let xdg = |var: &str, fallback: &str| {
            let base = std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| Path::new(&home).join(fallback));
            migrate(&base.join("veil"), &base.join("qveil"));
            base.join("qveil")
        };
        Ok(Self {
            config: xdg("XDG_CONFIG_HOME", ".config").join("config.toml"),
            data: xdg("XDG_DATA_HOME", ".local/share"),
            cache: xdg("XDG_CACHE_HOME", ".cache"),
        })
    }

    pub fn subscriptions(&self) -> PathBuf {
        self.data.join("subscriptions.json")
    }
    pub fn history(&self) -> PathBuf {
        self.data.join("history.json")
    }
    pub fn feed_cache(&self) -> PathBuf {
        self.cache.join("feed.json")
    }
    /// What processes lock to take turns changing the files above.
    pub fn lock_file(&self) -> PathBuf {
        self.data.join("lock")
    }
    /// A yt-dlp cache that isn't shared with any other yt-dlp use on the
    /// machine, so nothing from a logged-in session can bleed in.
    pub fn ytdlp_cache(&self) -> PathBuf {
        self.cache.join("yt-dlp")
    }
}

/// The app was called veil before it was qveil. Its directories move over
/// once, so subscriptions and settings survive the rename; a qveil directory
/// that already exists is never touched.
fn migrate(old: &Path, new: &Path) {
    if old.is_dir()
        && !new.exists()
        && let Err(e) = fs::rename(old, new)
    {
        eprintln!(
            "qveil: couldn't move {} to {}: {e}",
            old.display(),
            new.display()
        );
    }
    // The window's own settings file was named after the app too.
    let (old_conf, new_conf) = (new.join("veil.conf"), new.join("qveil.conf"));
    if old_conf.is_file() && !new_conf.exists() {
        let _ = fs::rename(old_conf, new_conf);
    }
}

pub fn ensure_dir(dir: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

pub fn load<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Like `load`, for data that can be rebuilt, such as the feed cache. A file
/// that doesn't parse (cut short by a crash, say) counts as empty and is
/// reported, instead of failing every command until it is deleted by hand.
/// Subscriptions and history use `load`, which errors, because replacing them
/// with nothing would silently destroy the user's data.
pub fn load_cache<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match load(path) {
        Err(e) if e.root_cause().is::<serde_json::Error>() => {
            eprintln!("qveil: ignoring unreadable cache ({e:#}); it will be rebuilt");
            Ok(T::default())
        }
        other => other,
    }
}

/// Held while changing the stored files, so that two processes (the app's
/// engine and a command-line run, say) don't each read the old contents and
/// then overwrite each other's changes. Released when dropped.
#[must_use = "the lock is released as soon as this is dropped"]
pub struct StoreLock {
    _file: File,
}

/// Waits for an advisory lock on a file in the data directory. Take it around
/// a whole read-modify-write, and not around network requests. Locks held on
/// separately opened files exclude each other even inside one process, so
/// this also orders threads. It isn't reentrant: don't take it twice.
pub fn lock(paths: &Paths) -> Result<StoreLock> {
    ensure_dir(&paths.data)?;
    let path = paths.lock_file();
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))?;
    Ok(StoreLock { _file: file })
}

pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(value)?)
}

/// Atomic write: a temp file created with mode 0600, synced, then renamed over the target.
/// The temp name is unique (process ID and a random part) and in the target's
/// directory, so concurrent writers never share a temp file and the rename
/// stays on one filesystem.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    ensure_dir(dir)?;
    let name = path.file_name().context("path has no file name")?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(
        ".{}-{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let tmp = dir.join(tmp_name);
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|()| {
            fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
pub mod testing {
    use std::path::PathBuf;

    /// A scratch directory that is removed when dropped.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            let p = std::env::temp_dir().join(format!("qveil-test-{:016x}", rand::random::<u64>()));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }

        pub fn paths(&self) -> super::Paths {
            super::Paths {
                config: self.0.join("config").join("config.toml"),
                data: self.0.join("data"),
                cache: self.0.join("cache"),
            }
        }
    }

    /// Writes an executable shell script and returns its path. Exec'ing a
    /// file that another thread's `fork` still holds open for writing fails
    /// with ETXTBSY, which tests running in parallel can cause, so the script
    /// is tried out (it answers `--qveil-probe` at once) until it can run.
    pub fn script(path: &std::path::Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let text = format!("#!/bin/sh\n[ \"$1\" = --qveil-probe ] && exit 0\n{body}");
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        loop {
            match std::process::Command::new(path)
                .arg("--qveil-probe")
                .status()
            {
                Err(e) if e.raw_os_error() == Some(26) && started.elapsed().as_secs() < 5 => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                other => {
                    other.unwrap();
                    break;
                }
            }
        }
        path.display().to_string()
    }

    /// A yt-dlp that lists one video for any channel and counts its runs in
    /// `<dir>/runs`. `extra` is shell run first.
    pub fn listing_ytdlp(dir: &std::path::Path, extra: &str) -> String {
        // The video is named after the process, so that every channel has its own.
        let body = format!(
            "echo x >> '{runs}'\n{extra}\nprintf '{{\"entries\":[{{\"id\":\"v%010d\",\"title\":\"t\"}}]}}' $$\n",
            runs = dir.join("runs").display()
        );
        script(&dir.join("yt-dlp"), &body)
    }

    /// How often `listing_ytdlp` has run.
    pub fn listing_runs(dir: &std::path::Path) -> usize {
        std::fs::read_to_string(dir.join("runs")).map_or(0, |s| s.lines().count())
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TempDir;
    use super::*;
    use serde::Deserialize;
    use std::os::unix::fs::PermissionsExt;

    #[derive(Default, Serialize, Deserialize, PartialEq, Debug)]
    struct Doc {
        n: u64,
    }

    #[test]
    fn concurrent_writers_never_trip_over_each_others_temp_files() {
        let dir = TempDir::new();
        let target = dir.0.join("feed.json");
        let writers: Vec<_> = (0..8u64)
            .map(|w| {
                let target = target.clone();
                std::thread::spawn(move || {
                    for i in 0..40 {
                        save(&target, &Doc { n: w * 1000 + i }).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let _: Doc = load(&target).unwrap();
        let left: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["feed.json"], "temp files must not be left behind");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_failed_write_leaves_no_temp_file_behind() {
        let dir = TempDir::new();
        let target = dir.0.join("sub.json");
        fs::create_dir(&target).unwrap(); // renaming a file over a directory fails
        assert!(write_atomic(&target, b"x").is_err());
        let names: Vec<_> = fs::read_dir(&dir.0).unwrap().collect();
        assert_eq!(names.len(), 1, "only the directory is left");
    }

    #[test]
    fn a_broken_cache_is_empty_but_broken_user_data_is_an_error() {
        let dir = TempDir::new();
        let file = dir.0.join("data.json");
        for broken in ["", "{", r#"{"n":1"#, "not json", r#"{"n":"x"}"#] {
            fs::write(&file, broken).unwrap();
            assert_eq!(
                load_cache::<Doc>(&file).unwrap(),
                Doc::default(),
                "{broken:?}"
            );
            assert!(load::<Doc>(&file).is_err(), "{broken:?}");
            assert_eq!(
                fs::read_to_string(&file).unwrap(),
                broken,
                "load must not touch it"
            );
        }
        // Not being able to read it at all is still an error, not an empty cache.
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        assert!(load_cache::<Doc>(&file).is_err());
        assert_eq!(
            load_cache::<Doc>(&dir.0.join("missing")).unwrap(),
            Doc::default()
        );
    }

    #[test]
    fn the_lock_makes_read_modify_write_atomic() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let counter = paths.data.join("counter.json");
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let (paths, counter) = (paths.clone(), counter.clone());
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        let _lock = lock(&paths).unwrap();
                        let mut doc: Doc = load(&counter).unwrap();
                        // Without the lock, others read the same value meanwhile.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        doc.n += 1;
                        save(&counter, &doc).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(load::<Doc>(&counter).unwrap().n, 60);
        assert_eq!(
            fs::metadata(paths.lock_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn the_old_veil_directories_move_over_once() {
        let root =
            std::env::temp_dir().join(format!("qveil-migrate-{:016x}", rand::random::<u64>()));
        let (old, new) = (root.join("veil"), root.join("qveil"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("subscriptions.json"), "{}").unwrap();
        fs::write(old.join("veil.conf"), "x").unwrap();
        migrate(&old, &new);
        assert!(!old.exists());
        assert!(new.join("subscriptions.json").is_file());
        assert!(
            new.join("qveil.conf").is_file(),
            "the window's settings file is renamed too"
        );

        // An existing qveil directory is never overwritten.
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("subscriptions.json"), "stale").unwrap();
        migrate(&old, &new);
        assert_eq!(
            fs::read_to_string(new.join("subscriptions.json")).unwrap(),
            "{}"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
