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
        let xdg = |var: &str, fallback: &str, held: Held| {
            let base = std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| Path::new(&home).join(fallback));
            migrate(&base.join("veil"), &base.join("qveil"), held);
            base.join("qveil")
        };
        Ok(Self {
            config: xdg("XDG_CONFIG_HOME", ".config", Held::Settings).join("config.toml"),
            data: xdg("XDG_DATA_HOME", ".local/share", Held::Settings),
            cache: xdg("XDG_CACHE_HOME", ".cache", Held::Cache),
        })
    }

    /// The directory the config file is in.
    pub fn config_dir(&self) -> &Path {
        self.config.parent().unwrap_or(&self.config)
    }

    /// What is left of the old name: `veil` directories beside qveil's own
    /// that migration didn't empty (see `migrate`), and that are still there.
    pub fn legacy(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = [self.config_dir(), &self.data, &self.cache]
            .into_iter()
            .map(|dir| dir.with_file_name("veil"))
            .filter(|old| fs::symlink_metadata(old).is_ok())
            .collect();
        found.dedup();
        found
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

/// What an old `veil` directory held, which decides what becomes of it.
#[derive(Clone, Copy, PartialEq)]
enum Held {
    /// Subscriptions and settings: kept, moved over to qveil.
    Settings,
    /// A cache: regenerable, so never moved on top of an existing qveil one.
    Cache,
}

/// The app was called veil before it was qveil. Its directories move over
/// once, so subscriptions and settings survive the rename.
///
/// The window starts first and creates its own qveil directories (with its
/// settings file in the config one), so by the time the engine runs a qveil
/// directory often exists already. Settings are then merged: whatever the old
/// directory has that the new one hasn't is moved, and nothing is overwritten.
/// An old cache is deleted instead: it can be rebuilt, and it records what
/// was browsed and watched, which is not worth keeping around.
fn migrate(old: &Path, new: &Path, held: Held) {
    let is_dir = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    let exists = |p: &Path| fs::symlink_metadata(p).is_ok();
    let warn = |what: &str, path: &Path, e: std::io::Error| {
        eprintln!("qveil: couldn't {what} {}: {e}", path.display());
    };
    if is_dir(old) {
        if !exists(new) {
            if let Err(e) = fs::rename(old, new) {
                warn("move", old, e);
            }
        } else if held == Held::Cache {
            if let Err(e) = fs::remove_dir_all(old) {
                warn("remove the old cache", old, e);
            }
        } else if let Ok(entries) = fs::read_dir(old) {
            for entry in entries.flatten() {
                // The window's own settings file was named after the app too.
                let name = match entry.file_name().to_str() {
                    Some("veil.conf") => "qveil.conf".into(),
                    _ => entry.file_name(),
                };
                let target = new.join(name);
                if !exists(&target)
                    && let Err(e) = fs::rename(entry.path(), &target)
                {
                    warn("move", &entry.path(), e);
                }
            }
            // Only succeeds if nothing was left behind.
            let _ = fs::remove_dir(old);
        }
    }
    let (old_conf, new_conf) = (new.join("veil.conf"), new.join("qveil.conf"));
    if old_conf.is_file() && !exists(&new_conf) {
        let _ = fs::rename(old_conf, new_conf);
    }
}

/// Brings qveil's directories to mode 0700, and the files directly in them to
/// 0600. `ensure_dir` only sets a mode on directories it creates itself, and
/// the window creates `~/.cache/qveil` and `~/.config/qveil` first, with the
/// default 0755 and 0644, which would let any local user read what you
/// subscribe to and what you watch. Symbolic links are left alone: what they
/// point at isn't qveil's to change.
pub fn secure(paths: &Paths) {
    for dir in [paths.config_dir(), &paths.data, &paths.cache] {
        if !restrict(dir, 0o700) {
            continue;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            restrict(&entry.path(), 0o600);
        }
    }
}

/// Sets the mode of a directory (when `mode` is 0700) or a regular file (any
/// other). Returns whether there was one to set; says so if it can't.
fn restrict(path: &Path, mode: u32) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    let right_kind = if mode == 0o700 {
        meta.is_dir()
    } else {
        meta.is_file()
    };
    if !right_kind {
        return false;
    }
    if meta.permissions().mode() & 0o7777 != mode
        && let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(mode))
    {
        eprintln!("qveil: couldn't restrict {}: {e}", path.display());
    }
    true
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

    /// A stand-in for YouTube's feed endpoint on a local port: the first
    /// `fail` requests get a 500 and the rest a feed with one video, named
    /// after the request's number. Returns its base URL (for
    /// `Runtime::feed_base`) and the number of requests it has had.
    pub fn rss_upstream(fail: usize) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&hits);
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                while r.read_line(&mut String::new()).is_ok_and(|n| n > 2) {}
                let n = counted.fetch_add(1, Ordering::SeqCst) + 1;
                let channel = line.split("channel_id=").nth(1).unwrap_or("");
                let channel = channel.split_whitespace().next().unwrap_or("");
                let (status, body) = if n <= fail {
                    ("500 Internal Server Error", String::new())
                } else {
                    let feed = format!(
                        "<feed xmlns:yt=\"http://www.youtube.com/xml/schemas/2015\" xmlns=\"http://www.w3.org/2005/Atom\"><entry><yt:videoId>v{n:010}</yt:videoId><yt:channelId>{channel}</yt:channelId><title>t{n}</title><published>2024-01-01T00:00:00+00:00</published></entry></feed>"
                    );
                    ("200 OK", feed)
                };
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (base, hits)
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
        let root = TempDir::new();
        let (old, new) = (root.0.join("veil"), root.0.join("qveil"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("subscriptions.json"), "{}").unwrap();
        fs::write(old.join("veil.conf"), "x").unwrap();
        migrate(&old, &new, Held::Settings);
        assert!(!old.exists());
        assert!(new.join("subscriptions.json").is_file());
        assert!(
            new.join("qveil.conf").is_file(),
            "the window's settings file is renamed too"
        );

        // An existing qveil file is never overwritten.
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("subscriptions.json"), "stale").unwrap();
        migrate(&old, &new, Held::Settings);
        assert_eq!(
            fs::read_to_string(new.join("subscriptions.json")).unwrap(),
            "{}"
        );
        assert!(
            old.join("subscriptions.json").exists(),
            "and isn't lost either"
        );
    }

    #[test]
    fn settings_are_merged_into_a_directory_the_window_created_first() {
        let root = TempDir::new();
        let (old, new) = (root.0.join("veil"), root.0.join("qveil"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("config.toml"), "proxy").unwrap();
        fs::write(old.join("veil.conf"), "old window settings").unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("qveil.conf"), "new window settings").unwrap();
        migrate(&old, &new, Held::Settings);
        assert_eq!(
            fs::read_to_string(new.join("config.toml")).unwrap(),
            "proxy"
        );
        assert_eq!(
            fs::read_to_string(new.join("qveil.conf")).unwrap(),
            "new window settings"
        );

        // Nothing left to move means nothing left behind.
        let (old, new) = (root.0.join("veil2"), root.0.join("qveil2"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("subscriptions.json"), "{}").unwrap();
        fs::create_dir_all(&new).unwrap();
        migrate(&old, &new, Held::Settings);
        assert!(new.join("subscriptions.json").is_file() && !old.exists());
    }

    #[test]
    fn an_old_cache_is_deleted_when_the_new_one_exists_and_moved_otherwise() {
        let root = TempDir::new();
        let (old, new) = (root.0.join("veil"), root.0.join("qveil"));
        fs::create_dir_all(old.join("thumbs")).unwrap();
        fs::write(old.join("thumbs").join("watched.jpg"), "x").unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("feed.json"), "{}").unwrap();
        migrate(&old, &new, Held::Cache);
        assert!(!old.exists(), "the old cache records what was watched");
        assert!(new.join("feed.json").is_file(), "the new one is untouched");

        // With no new cache yet, the old one is moved, as before.
        let (old, new) = (root.0.join("veil-b"), root.0.join("qveil-b"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("feed.json"), "{}").unwrap();
        migrate(&old, &new, Held::Cache);
        assert!(new.join("feed.json").is_file() && !old.exists());
    }

    #[test]
    fn the_directories_and_their_files_are_made_private() {
        let root = TempDir::new();
        let paths = root.paths();
        let config_dir = paths.config_dir().to_path_buf();
        let shared = |p: &Path, mode| {
            if p.extension().is_some() {
                fs::write(p, "x").unwrap();
            } else {
                fs::create_dir_all(p).unwrap();
            }
            fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
        };
        // What the window leaves behind: world-readable.
        shared(&config_dir, 0o755);
        shared(&paths.data, 0o755);
        shared(&paths.cache, 0o750);
        shared(&paths.config, 0o644);
        shared(&config_dir.join("qveil.conf"), 0o664);
        shared(&paths.subscriptions(), 0o644);
        // A symbolic link is not followed to change what it points at.
        let outside = root.0.join("dotfiles.toml");
        shared(&outside, 0o644);
        std::os::unix::fs::symlink(&outside, config_dir.join("linked.toml")).unwrap();

        secure(&paths);
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        for dir in [&config_dir, &paths.data, &paths.cache] {
            assert_eq!(mode(dir), 0o700, "{}", dir.display());
        }
        for file in [
            &paths.config,
            &config_dir.join("qveil.conf"),
            &paths.subscriptions(),
        ] {
            assert_eq!(mode(file), 0o600, "{}", file.display());
        }
        assert_eq!(mode(&outside), 0o644);
        // Missing directories are left missing, and a second run changes nothing.
        let nothing = TempDir::new();
        secure(&nothing.paths());
        assert!(!nothing.paths().data.exists());
        secure(&paths);
        assert_eq!(mode(&config_dir), 0o700);
    }

    #[test]
    fn leftover_veil_directories_are_found_beside_the_new_ones() {
        let root = TempDir::new();
        let paths = Paths {
            config: root.0.join("config/qveil/config.toml"),
            data: root.0.join("data/qveil"),
            cache: root.0.join("cache/qveil"),
        };
        assert!(paths.legacy().is_empty());
        fs::create_dir_all(root.0.join("config/veil")).unwrap();
        fs::create_dir_all(root.0.join("cache/veil")).unwrap();
        assert_eq!(
            paths.legacy(),
            [root.0.join("config/veil"), root.0.join("cache/veil")]
        );
    }
}
