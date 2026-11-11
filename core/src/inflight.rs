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
use crate::spare::{self, Launch};
use crate::util::lock as locked;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
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
    /// Becomes readable when the process ends (a pidfd), so that waiting for
    /// that needs neither the lock nor a poll. `None` where the kernel has no
    /// pidfds; waiting then polls.
    exit: Option<OwnedFd>,
    /// Where its private files are, which `close` removes when the engine is
    /// exiting. A spare has none until a call takes it.
    dir: OnceLock<PathBuf>,
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

    /// Waits up to `limit` for the process to end, without the lock (so that
    /// `kill` can take it meanwhile) and without reaping it. True once it
    /// has ended: `try_wait` then has its status.
    pub fn wait_exit(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        if let Some(fd) = &self.exit {
            // On an error, it falls back to polling for what is left of the time.
            if let Ok(ended) = proc::wait_readable(fd, limit) {
                return ended;
            }
        }
        loop {
            if !matches!(self.try_wait(), Ok(None)) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            std::thread::sleep(POLL.min(deadline - now));
        }
    }

    /// Waits for it to end. `None` if it is still there after `limit`.
    fn wait_up_to(&self, limit: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + limit;
        loop {
            match self.try_wait() {
                Ok(Some(status)) => return Some(status),
                // Nothing more to wait for.
                Err(_) => return None,
                Ok(None) => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            self.wait_exit(deadline - now);
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
    /// The spare yt-dlp of this route (see `spare`).
    spares: Mutex<Spares>,
    /// Wakes the thread that starts the spares.
    spares_changed: Condvar,
}

#[derive(Default)]
struct Spares {
    /// What to keep a spare of, once a call has asked for one.
    want: Option<Launch>,
    /// The spare, once it has started and imported yt-dlp.
    ready: Option<Ready>,
    /// Whether the thread that starts them is running.
    keeper: bool,
    /// Set when a spare failed to start: there are no more, and calls start
    /// the usual way.
    broken: bool,
}

/// A spare that is waiting for its arguments. Its process is in
/// `State::running` like any other, so that a close stops it, and dropping
/// it ends the process.
struct Ready {
    launch: Launch,
    run: Run,
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
        let run = self.spawn(cmd)?;
        let _ = run.proc.dir.set(dir.to_path_buf());
        Ok(run)
    }

    /// `start`, without a directory yet.
    fn spawn(self: &Arc<Self>, cmd: &mut Command) -> Result<Run, StartError> {
        let mut state = locked(&self.state);
        if let Some(why) = state.closed {
            return Err(StartError::Closed(why));
        }
        let mut child = proc::spawn(cmd.stdout(Stdio::piped()).stderr(Stdio::piped()))
            .map_err(StartError::Spawn)?;
        let (stdin, stdout, stderr) =
            (child.take_stdin(), child.take_stdout(), child.take_stderr());
        // While the child is unreaped its id is its own, so this is the right process.
        let exit = proc::pidfd(child.pid());
        let proc = Arc::new(Proc {
            child: Mutex::new(child),
            exit,
            dir: OnceLock::new(),
            stopped: OnceLock::new(),
        });
        let id = state.next;
        state.next += 1;
        state.running.push((id, Arc::clone(&proc)));
        Ok(Run {
            proc,
            calls: Arc::clone(self),
            id,
            stdin,
            stdout,
            stderr,
        })
    }

    /// Asks for a spare of `launch` to be kept from now on, if there isn't
    /// one. A spare of anything else is let go.
    pub fn want_spare(self: &Arc<Self>, launch: &Launch) {
        let mut sp = locked(&self.spares);
        if sp.broken || (sp.want.as_ref() == Some(launch) && sp.keeper) {
            return;
        }
        // Ending its process waits for it to go, which is for the end of
        // this function, once the lock that calls take spares by is free.
        let mut old = None;
        if sp.want.as_ref() != Some(launch) {
            sp.want = Some(launch.clone());
            old = sp.ready.take();
        }
        if !sp.keeper {
            let calls = Arc::clone(self);
            match std::thread::Builder::new().spawn(move || calls.keep_spares()) {
                Ok(_) => sp.keeper = true,
                Err(_) => sp.broken = true,
            }
        }
        self.spares_changed.notify_all();
        drop(sp);
        drop(old);
    }

    /// Whether a spare is ready to take a call.
    #[cfg(test)]
    pub fn spare_ready(&self) -> bool {
        locked(&self.spares).ready.is_some()
    }

    /// What a spare is kept of, if anything.
    pub fn spare_wanted(&self) -> Option<Launch> {
        locked(&self.spares).want.clone()
    }

    /// Hands `line` (the call's arguments, see `spare::args_line`) to the
    /// spare of `launch`, if there is one ready, and returns it as the
    /// call's process. `None` if there is none, and the call is to be
    /// started the usual way.
    pub fn claim_spare(self: &Arc<Self>, launch: &Launch, line: &str, dir: &Path) -> Option<Run> {
        let ready = {
            let mut sp = locked(&self.spares);
            if sp.ready.as_ref()?.launch != *launch {
                return None;
            }
            let ready = sp.ready.take();
            // The keeper starts the next one.
            self.spares_changed.notify_all();
            ready?
        };
        let mut run = ready.run;
        // One that has died is dropped, and so killed and reaped. A close
        // after this point stops it like any running call.
        if !matches!(run.try_wait(), Ok(None)) || locked(&self.state).closed.is_some() {
            return None;
        }
        let _ = run.proc.dir.set(dir.to_path_buf());
        // Closed after the line, so that nothing else can reach it.
        let mut stdin = run.stdin.take()?;
        stdin.write_all(line.as_bytes()).ok()?;
        drop(stdin);
        Some(run)
    }

    /// The thread that starts the spares. Started by the first `want_spare`,
    /// it is the thread that spawns them, and must outlive them: a child is
    /// killed when the *thread* that started it ends (see `proc::Group`). So
    /// it ends only once the registry is closed and they have all ended.
    fn keep_spares(self: Arc<Self>) {
        let mut started: Vec<Arc<Proc>> = Vec::new();
        loop {
            let launch = {
                let mut sp = locked(&self.spares);
                loop {
                    if locked(&self.state).closed.is_some() || sp.broken {
                        break None;
                    }
                    if sp.ready.is_none()
                        && let Some(want) = &sp.want
                    {
                        break Some(want.clone());
                    }
                    sp = match self.spares_changed.wait_timeout(sp, Duration::from_secs(1)) {
                        Ok((sp, _)) => sp,
                        Err(e) => e.into_inner().0,
                    };
                }
            };
            let Some(launch) = launch else { break };
            started.retain(|p| !p.wait_exit(Duration::ZERO));
            let Ok(mut run) = self.spawn(&mut launch.command()) else {
                locked(&self.spares).broken = true;
                break;
            };
            started.push(Arc::clone(&run.proc));
            if !ready(&mut run) {
                locked(&self.spares).broken = true;
                break;
            }
            let mut sp = locked(&self.spares);
            if sp.want.as_ref() == Some(&launch) && sp.ready.is_none() {
                sp.ready = Some(Ready { launch, run });
            }
        }
        let gone = locked(&self.spares).ready.take();
        drop(gone);
        for p in started {
            while !p.wait_exit(Duration::from_secs(60)) {}
        }
        locked(&self.spares).keeper = false;
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
        // The spare's keeper lets it go and ends.
        self.spares_changed.notify_all();
        for p in &procs {
            p.stop(why);
        }
        if why == Why::Exiting {
            for p in &procs {
                p.wait_up_to(REAP);
                // Doesn't follow symbolic links; a replaced directory is just unlinked.
                if let Some(dir) = p.dir.get() {
                    let _ = std::fs::remove_dir_all(dir);
                }
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
    stdin: Option<ChildStdin>,
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
            self.proc.wait_exit(Duration::from_secs(60));
        }
    }

    /// See `Proc::wait_exit`.
    pub fn wait_exit(&self, limit: Duration) -> bool {
        self.proc.wait_exit(limit)
    }

    /// Set when the registry killed the process while it was running (a route
    /// change, the engine exiting), as opposed to it ending by itself or at
    /// its owner's request. What it printed is then moot, and so is its exit status.
    pub fn stopped(&self) -> Option<Why> {
        self.proc.stopped.get().copied()
    }
}

/// Waits until a spare that was just started has imported yt-dlp and says
/// so. False if it ended first, or said anything else.
fn ready(run: &mut Run) -> bool {
    let Some(stderr) = run.stderr.as_mut() else {
        return false;
    };
    let mut said = Vec::new();
    let mut byte = [0u8];
    // Byte by byte, so that nothing after the line is taken from the call
    // that gets the pipe.
    while said.len() < spare::READY.len() && !said.ends_with(b"\n") {
        match stderr.read(&mut byte) {
            Ok(1) => said.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            _ => return false,
        }
    }
    said == spare::READY
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
    fn waiting_for_the_end_wakes_up_as_it_ends_and_leaves_reaping_to_the_owner() {
        let dir = TempDir::new();
        let calls = Arc::new(Inflight::default());
        let mut short = Command::new("sleep");
        short.arg("0.3").stdin(Stdio::null());
        let run = calls.start(&mut short, &dir.0).unwrap();
        let started = Instant::now();
        assert!(run.wait_exit(Duration::from_secs(5)));
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(250) && took < Duration::from_millis(300 + 50),
            "{took:?}"
        );
        // Not reaped by the wait: the status is still there for the owner.
        assert!(run.try_wait().unwrap().unwrap().success());

        // One that goes on running is waited for no longer than asked.
        let run = calls.start(&mut sleeper(&dir), &dir.0).unwrap();
        let started = Instant::now();
        assert!(!run.wait_exit(Duration::from_millis(100)));
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(100) && took < Duration::from_secs(1),
            "{took:?}"
        );
        // And a kill from elsewhere ends the wait at once.
        let handle = run.handle();
        let killer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            handle.kill();
        });
        let started = Instant::now();
        assert!(run.wait_exit(Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(1));
        killer.join().unwrap();
    }

    /// Puts a spare of `launch`, ready, in the registry, as if a keeper had
    /// started it (one is marked as running, so none is). Returns its process.
    fn ready_spare(calls: &Arc<Inflight>, launch: &Launch, dir: &TempDir) -> u32 {
        let run = calls.start(&mut sleeper(dir), &dir.0).unwrap();
        let pid = run.proc.child.lock().unwrap().id();
        let mut sp = locked(&calls.spares);
        sp.want = Some(launch.clone());
        sp.keeper = true;
        sp.ready = Some(Ready {
            launch: launch.clone(),
            run,
        });
        pid
    }

    #[test]
    fn a_spare_that_is_swapped_out_is_ended_without_the_lock_that_calls_take_spares_by() {
        let dir = TempDir::new();
        let calls = Arc::new(Inflight::default());
        let (old, new) = (Launch::testing("old"), Launch::testing("new"));
        let pid = ready_spare(&calls, &old, &dir);
        // The last thing that ending a run does is take the registry's state
        // lock, so holding it keeps the swap from finishing for as long as
        // the test likes.
        let held = locked(&calls.state);
        let swapper = {
            let calls = Arc::clone(&calls);
            std::thread::spawn(move || calls.want_spare(&new))
        };
        // It has killed the old spare by now, and is stuck on the state lock.
        let started = Instant::now();
        while !gone(pid) {
            assert!(started.elapsed() < Duration::from_secs(5), "not swapped");
            std::thread::sleep(Duration::from_millis(5));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let claimer = {
            let calls = Arc::clone(&calls);
            let dir = dir.0.clone();
            std::thread::spawn(move || {
                let _ = tx.send(calls.claim_spare(&old, "[]\n", &dir).is_none());
            })
        };
        // A call that comes for the spare meanwhile finds none, at once.
        let answered = rx.recv_timeout(Duration::from_secs(5));
        drop(held);
        swapper.join().unwrap();
        claimer.join().unwrap();
        assert_eq!(answered, Ok(true), "claim_spare waited for the swap");
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
