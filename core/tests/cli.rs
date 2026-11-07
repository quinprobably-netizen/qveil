//! The real binary, with its own XDG directories. Nothing here touches the
//! network: every command is one that only reads or writes local files.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::time::{Duration, Instant};

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

/// Writes an executable shell script, and waits until it can be run: exec'ing
/// a file that another test thread's `fork` still holds open for writing
/// fails with ETXTBSY.
fn script(path: &Path, body: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!("#!/bin/sh\n[ \"$1\" = --qveil-probe ] && exit 0\n{body}"),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    let started = Instant::now();
    while let Err(e) = Command::new(path).arg("--qveil-probe").status() {
        assert!(
            e.raw_os_error() == Some(26) && started.elapsed() < Duration::from_secs(5),
            "{e}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    path.to_path_buf()
}

/// A running `qveil-core serve`, the way the window starts it.
struct Serve {
    child: Child,
    stdin: Option<ChildStdin>,
    base: String,
}

impl Home {
    /// Starts the server with `ytdlp` as its yt-dlp, and the private
    /// directories (which `$XDG_RUNTIME_DIR` decides) inside this home.
    fn serve(&self, ytdlp: &Path) -> Serve {
        write(
            &self.config().join("qveil/config.toml"),
            &format!("ytdlp = {:?}\n", ytdlp.display().to_string()),
        );
        let runtime = self.0.join("run");
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_qveil-core"))
            .arg("serve")
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.config())
            .env("XDG_DATA_HOME", self.data())
            .env("XDG_CACHE_HOME", self.cache())
            .env("XDG_RUNTIME_DIR", &runtime)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        let base = line
            .split('"')
            .find(|part| part.starts_with("http://"))
            .unwrap_or_else(|| panic!("no base URL in {line:?}"))
            .to_string();
        Serve { child, stdin, base }
    }
}

impl Serve {
    /// Sends `GET path` (below the token) on a thread of its own and returns
    /// at once: the reply is whatever the engine says before it ends.
    fn get_in_background(&self, path: &str) -> std::thread::JoinHandle<String> {
        let (addr, token) = {
            let rest = self.base.strip_prefix("http://").unwrap();
            let (addr, token) = rest.split_once('/').unwrap();
            (addr.to_string(), token.to_string())
        };
        let path = path.to_string();
        std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
            let _ = write!(c, "GET /{token}/{path} HTTP/1.1\r\nHost: x\r\n\r\n");
            let mut reply = String::new();
            let _ = c.read_to_string(&mut reply);
            reply
        })
    }

    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the engine didn't exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A yt-dlp that records its process id and the directory of its private
/// files, and then hangs: a search still waiting on YouTube.
fn hanging_ytdlp(dir: &Path) -> PathBuf {
    script(
        &dir.join("yt-dlp"),
        r#"d=$(dirname "$0")
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && dirname "$2" > "$d/private-dir"; shift; done
echo $$ > "$d/pid"
exec sleep 60
"#,
    )
}

/// What `hanging_ytdlp` recorded, once it has recorded all of it.
fn wait_for_hanging(dir: &Path) -> (String, PathBuf) {
    let started = Instant::now();
    loop {
        let read = |name: &str| fs::read_to_string(dir.join(name)).ok();
        if let (Some(pid), Some(private)) = (read("pid"), read("private-dir"))
            && pid.ends_with('\n')
            && private.ends_with('\n')
        {
            return (pid.trim().into(), private.trim().into());
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "yt-dlp never started"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_gone(pid: &str, private: &Path) {
    // Gone for good, not a zombie: the engine waited for it before exiting.
    // (A moment's grace for the kernel to finish with the process table.)
    let started = Instant::now();
    while Path::new("/proc").join(pid).exists() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "yt-dlp ({pid}) is still running"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!private.exists(), "{} was left behind", private.display());
}

#[test]
fn closing_stdin_stops_yt_dlp_and_removes_its_private_files() {
    let home = Home::new();
    let bin = home.0.join("bin");
    let mut serve = home.serve(&hanging_ytdlp(&bin));
    let search = serve.get_in_background("api/search?q=anything");
    let (pid, private) = wait_for_hanging(&bin);
    assert!(private.is_dir());
    // The config file (with the proxy's password, on a proxied route) is there.
    assert!(private.join("yt-dlp.conf").is_file());

    // The window going away closes our stdin.
    drop(serve.stdin.take());
    let status = serve.wait_for_exit();
    assert!(status.success(), "{status}");
    assert_gone(&pid, &private);
    let _ = search.join();
}

#[test]
fn a_termination_signal_stops_yt_dlp_removes_its_private_files_and_ends_by_that_signal() {
    for (name, number) in [("TERM", 15), ("INT", 2), ("HUP", 1)] {
        let home = Home::new();
        let bin = home.0.join("bin");
        let mut serve = home.serve(&hanging_ytdlp(&bin));
        let search = serve.get_in_background("api/search?q=anything");
        let (pid, private) = wait_for_hanging(&bin);

        let killed = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(serve.child.id().to_string())
            .status()
            .unwrap();
        assert!(killed.success());
        let status = serve.wait_for_exit();
        assert_eq!(status.signal(), Some(number), "{name}: {status}");
        assert_gone(&pid, &private);
        drop(serve.stdin.take());
        let _ = search.join();
    }
}
