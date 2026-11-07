//! The yt-dlp processes that are running right now.
//!
//! Every call starts through `Inflight::start`, so that all of them can be
//! stopped together. Two things need that. The engine's exit (the window
//! closed, or a signal) must not leave yt-dlp running with the proxy's
//! password in its config file. And a route change must not let a search that
//! began on the old route carry on over it: each route has its own `Inflight`
//! (shared by every settings change that keeps the route), which is closed
//! when the route changes, so that nothing more can start on it either.

use crate::proc::{self, Group};
use crate::util::lock as locked;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What the API says to a request whose work was cut off by a route change.
pub const ROUTE_CHANGED: &str = "The route changed; try again";

/// How long a stopped process is given to disappear. SIGKILL can't be
/// refused, so this only bounds a wait on a process the kernel is slow to reap.
const REAP: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(10);

/// Why running processes were stopped, and why no more may start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The route they were started on is no longer the one in use.
    RouteChanged,
    /// The engine is exiting.
    Exiting,
}

impl Why {
    pub fn message(self) -> &'static str {
        match self {
            Why::RouteChanged => ROUTE_CHANGED,
            Why::Exiting => "qveil is shutting down",
        }
    }
}

/// Why `Inflight::start` didn't start anything.
#[derive(Debug)]
pub enum StartError {
    /// The registry is closed.
    Closed(Why),
    /// The program couldn't be started.
    Spawn(io::Error),
}

/// A process that the registry can stop and its owner can wait for. Shared by
/// both, which is why the child is behind a lock that is never held while waiting.
pub struct Proc {
    child: Mutex<Group>,
    /// Where its private files are, which `close` removes when the engine is exiting.
    dir: PathBuf,
    /// Set (once) by the registry when it stopped the process, so its owner
    /// can tell that apart from the process failing or finishing by itself.
    stopped: OnceLock<Why>,
}

impl Proc {
    /// Kills the process and everything it started (its process group).
    pub fn kill(&self) {
        locked(&self.child).kill();
    }

    fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        locked(&self.child).try_wait()
    }

    /// Waits for it to end, polling so that `kill` can take the lock in
    /// between (a blocking `wait` would hold it until the process ended on
    /// its own). `None` if it is still there after `limit`.
    fn wait_up_to(&self, limit: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + limit;
        loop {
            match self.try_wait() {
                Ok(Some(status)) => return Some(status),
                // Nothing more to wait for.
                Err(_) => return None,
                Ok(None) if Instant::now() >= deadline => return None,
                Ok(None) => std::thread::sleep(POLL),
            }
        }
    }

    /// Kills the process if it is still running, and records why. One that
    /// had finished already isn't marked: its result was complete before the
    /// registry closed (see `Run::stopped`).
    fn stop(&self, why: Why) {
        let mut child = locked(&self.child);
        if matches!(child.try_wait(), Ok(None)) {
            let _ = self.stopped.set(why);
            child.kill();
        }
    }
}

/// The processes of one route.
#[derive(Default)]
pub struct Inflight {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Once set, nothing more starts.
    closed: Option<Why>,
    running: Vec<(u64, Arc<Proc>)>,
    next: u64,
}

impl Inflight {
    /// Starts `cmd` (with its output piped) unless the registry is closed.
    /// `dir` is the private directory of the files it was given. The check and
    /// the spawn happen under one lock, so a process either starts before
    /// `close` and is stopped by it, or doesn't start at all.
    pub fn start(self: &Arc<Self>, cmd: &mut Command, dir: &Path) -> Result<Run, StartError> {
        let mut state = locked(&self.state);
        if let Some(why) = state.closed {
            return Err(StartError::Closed(why));
        }
        let mut child = proc::spawn(cmd.stdout(Stdio::piped()).stderr(Stdio::piped()))
            .map_err(StartError::Spawn)?;
        let (stdout, stderr) = (child.take_stdout(), child.take_stderr());
        let proc = Arc::new(Proc {
            child: Mutex::new(child),
            dir: dir.to_path_buf(),
            stopped: OnceLock::new(),
        });
        let id = state.next;
        state.next += 1;
        state.running.push((id, Arc::clone(&proc)));
        Ok(Run {
            proc,
            calls: Arc::clone(self),
            id,
            stdout,
            stderr,
        })
    }

    /// Stops every running process and refuses new ones. For `Why::Exiting`
    /// it also waits until they are gone and removes their private
    /// directories, because the caller is about to end the process, and
    /// nothing else would. (Otherwise the owner of each process does that
    /// when it notices.)
    pub fn close(&self, why: Why) {
        let procs: Vec<Arc<Proc>> = {
            let mut state = locked(&self.state);
            state.closed.get_or_insert(why);
            state.running.iter().map(|(_, p)| Arc::clone(p)).collect()
        };
        for p in &procs {
            p.stop(why);
        }
        if why == Why::Exiting {
            for p in &procs {
                p.wait_up_to(REAP);
                // Doesn't follow symbolic links; a replaced directory is just unlinked.
                let _ = std::fs::remove_dir_all(&p.dir);
            }
        }
    }

    #[cfg(test)]
    pub fn running(&self) -> usize {
        locked(&self.state).running.len()
    }
}

/// A process started through the registry. Stops the process, if it is still
/// running, when dropped, so an error path can't leave one behind.
pub struct Run {
    proc: Arc<Proc>,
    calls: Arc<Inflight>,
    id: u64,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

impl Run {
    /// A handle that can kill the process from another thread (a deadline).
    pub fn handle(&self) -> Arc<Proc> {
        Arc::clone(&self.proc)
    }

    pub fn kill(&self) {
        self.proc.kill();
    }

    pub fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        self.proc.try_wait()
    }

    /// Waits for the process to end. Whoever killed it (the registry, a
    /// deadline) has done so before this is called, or does so meanwhile.
    pub fn wait(&self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.proc.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(POLL);
        }
    }

    /// Set when the registry killed the process while it was running (a route
    /// change, the engine exiting), as opposed to it ending by itself or at
    /// its owner's request. What it printed is then moot, and so is its exit status.
    pub fn stopped(&self) -> Option<Why> {
        self.proc.stopped.get().copied()
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.proc.kill();
        self.proc.wait_up_to(REAP);
        locked(&self.calls.state)
            .running
            .retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempDir;

    fn sleeper(dir: &TempDir) -> Command {
        let mut c = Command::new("sleep");
        c.arg("60").current_dir(&dir.0).stdin(Stdio::null());
        c
    }

    fn gone(pid: u32) -> bool {
        // A killed child that hasn't been waited for is a zombie; its state in
        // /proc says so, and it no longer runs.
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| s.rsplit(") ").next().is_some_and(|r| r.starts_with('Z')))
            .unwrap_or(true)
    }

    #[test]
    fn closing_for_a_route_change_stops_what_runs_and_refuses_the_rest() {
        let dir = TempDir::new();
        let calls = Arc::new(Inflight::default());
        let run = calls.start(&mut sleeper(&dir), &dir.0).unwrap();
        let pid = run.proc.child.lock().unwrap().id();
        assert_eq!((calls.running(), run.stopped()), (1, None));

        calls.close(Why::RouteChanged);
        run.wait().unwrap();
        assert_eq!(run.stopped(), Some(Why::RouteChanged));
        assert!(gone(pid));
        // Nothing more starts on the route that was left.
        let refused = calls.start(&mut sleeper(&dir), &dir.0).err();
        assert!(matches!(
            refused,
            Some(StartError::Closed(Why::RouteChanged))
        ));
        // The owner is the one that removes its directory, in this case.
        assert!(dir.0.exists());
        drop(run);
        assert_eq!(calls.running(), 0);
    }

    #[test]
    fn closing_at_exit_waits_for_the_process_and_removes_its_directory() {
        let root = TempDir::new();
        let private = root.0.join("qveil-ytdlp-1-secret");
        std::fs::create_dir(&private).unwrap();
        std::fs::write(private.join("yt-dlp.conf"), "--proxy 'http://u:SECRET@h:1'").unwrap();
        let calls = Arc::new(Inflight::default());
        let run = calls.start(&mut sleeper(&root), &private).unwrap();
        let pid = run.proc.child.lock().unwrap().id();

        calls.close(Why::Exiting);
        // Without anyone else having waited for it: the process is reaped and
        // the directory is gone by the time `close` returns.
        assert!(!Path::new("/proc").join(pid.to_string()).exists());
        assert!(!private.exists());
        assert_eq!(run.stopped(), Some(Why::Exiting));
        assert_eq!(Why::Exiting.message(), "qveil is shutting down");
    }

    #[test]
    fn a_process_that_finished_before_the_close_is_not_blamed() {
        let dir = TempDir::new();
        let calls = Arc::new(Inflight::default());
        let run = calls
            .start(Command::new("true").stdin(Stdio::null()), &dir.0)
            .unwrap();
        let status = run.wait().unwrap();
        calls.close(Why::RouteChanged);
        assert!(status.success());
        // Its result was in before the route changed; only what was still
        // running is cut off (the API's own check covers a late reply).
        assert_eq!(run.stopped(), None);
    }

    #[test]
    fn dropping_a_run_stops_its_process() {
        let dir = TempDir::new();
        let calls = Arc::new(Inflight::default());
        let run = calls.start(&mut sleeper(&dir), &dir.0).unwrap();
        let pid = run.proc.child.lock().unwrap().id();
        drop(run);
        assert!(!Path::new("/proc").join(pid.to_string()).exists());
        assert_eq!(calls.running(), 0);
        // The registry stays open: this wasn't a close.
        assert!(calls.start(&mut sleeper(&dir), &dir.0).is_ok());
    }
}
