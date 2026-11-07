//! The real binary, with its own XDG directories. Nothing here touches the
//! network: every command is one that only reads or writes local files.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A scratch home for one test, removed afterwards.
struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let n = std::process::id() as u64 * 1_000_003
            + std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos() as u64;
        let dir = std::env::temp_dir().join(format!("qveil-cli-test-{n:x}"));
        fs::create_dir_all(&dir).unwrap();
        Home(dir)
    }

    fn config(&self) -> PathBuf {
        self.0.join("config")
    }
    fn data(&self) -> PathBuf {
        self.0.join("data")
    }
    fn cache(&self) -> PathBuf {
        self.0.join("cache")
    }

    /// Runs `qveil-core` with `args`, answering `stdin` if it asks.
    fn run(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_qveil-core"))
            .args(args)
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.config())
            .env("XDG_DATA_HOME", self.data())
            .env("XDG_CACHE_HOME", self.cache())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn serve_refuses_the_route_flags_that_the_first_settings_change_would_drop() {
    let home = Home::new();
    for flags in [&["--tor"][..], &["--proxy", "http://127.0.0.1:3128"]] {
        let args: Vec<&str> = flags.iter().copied().chain(["serve"]).collect();
        let out = home.run(&args, "");
        assert!(!out.status.success(), "{flags:?}");
        assert!(text(&out.stdout).is_empty(), "it must not start serving");
        let err = text(&out.stderr);
        assert!(
            err.contains("`serve` doesn't take --proxy or --tor"),
            "{err}"
        );
    }
    // Without them it starts (and, with its stdin already closed, stops again).
    let out = home.run(&["serve"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("\"base\""));
}

#[test]
fn the_directories_the_window_made_are_made_private_at_startup() {
    let home = Home::new();
    // What the window leaves behind: 0755 directories, 0644 files.
    let files = [
        home.config().join("qveil/config.toml"),
        home.config().join("qveil/qveil.conf"),
        home.data().join("qveil/subscriptions.json"),
        home.cache().join("qveil/feed.json"),
    ];
    for f in &files {
        write(f, "");
        fs::set_permissions(f, fs::Permissions::from_mode(0o644)).unwrap();
        let dir = f.parent().unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = home.run(&["config"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    for f in &files {
        assert_eq!(mode(f), 0o600, "{}", f.display());
        assert_eq!(mode(f.parent().unwrap()), 0o700, "{}", f.display());
    }
}

#[test]
fn an_old_cache_is_deleted_at_startup_when_the_new_one_exists() {
    let home = Home::new();
    write(&home.cache().join("veil/feed.json"), "what was watched");
    write(&home.cache().join("qveil/feed.json"), "{}");
    let out = home.run(&["config"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!home.cache().join("veil").exists());
    assert!(home.cache().join("qveil/feed.json").exists());
}

#[test]
fn wipe_also_removes_what_the_old_name_left_behind() {
    let home = Home::new();
    // Both names have a subscriptions file and a config, so the old ones
    // can't be merged away at startup and are left over.
    for dir in ["data/veil", "data/qveil"] {
        write(&home.0.join(dir).join("subscriptions.json"), "{}");
    }
    for dir in ["config/veil", "config/qveil"] {
        write(&home.0.join(dir).join("config.toml"), "");
    }
    write(&home.cache().join("qveil/feed.json"), "{}");

    // Declined: it lists everything, including the old directories, and removes nothing.
    let out = home.run(&["wipe"], "no\n");
    assert!(!out.status.success());
    let said = text(&out.stdout);
    for listed in [
        home.data().join("qveil"),
        home.data().join("veil"),
        home.config().join("veil"),
        home.cache().join("qveil"),
    ] {
        assert!(said.contains(&listed.display().to_string()), "{said}");
    }
    assert!(said.contains("left over"), "{said}");
    assert!(home.data().join("veil").exists());

    let out = home.run(&["wipe"], "wipe\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    for gone in [
        home.data().join("qveil"),
        home.data().join("veil"),
        home.config().join("veil"),
        home.cache().join("qveil"),
    ] {
        assert!(!gone.exists(), "{} is still there", gone.display());
    }
    assert!(
        home.config().join("qveil/config.toml").exists(),
        "the config file is kept"
    );
    assert!(text(&home.run(&["wipe", "--yes"], "").stdout).contains("nothing to wipe"));
}
