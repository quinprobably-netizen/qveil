//! The process tree belongs to the engine.
//!
//! Every child the engine starts (yt-dlp, mpv) goes through `spawn`, which
//! makes it the leader of a process group of its own and asks the kernel to
//! kill it when the engine dies. Killing then means the whole group
//! (`killpg`), so a deno that yt-dlp started for its JavaScript challenge goes
//! with it.
//!
//! `PR_SET_PDEATHSIG` only reaches the direct child, though: a grandchild is
//! not a child of the engine and has no such setting, so when the engine is
//! killed hard (SIGKILL, the OOM killer) it would be left running. That is
//! what the guard is for (see `guard`): a tiny watcher process that outlives
//! the engine by a moment and kills the groups it was told about.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use std::time::{Duration, Instant};

/// A child process and the group it leads.
///
/// The kernel ties `PR_SET_PDEATHSIG` to the *thread* that started the child,
/// not to the process: the child is killed when that thread ends. So the
/// thread that calls `spawn` must be the one that waits for the child, as
/// every caller in the engine does.
pub struct Group {
    child: Child,
    reaped: bool,
}

/// Starts `cmd` as the leader of its own process group, killed when the
/// engine dies. Its stdout and stderr are whatever `cmd` set.
pub fn spawn(cmd: &mut Command) -> io::Result<Group> {
    // Before the fork, so that the child can tell whether its parent was
    // already gone when it asked to be told about that.
    let parent = unsafe { libc::getpid() };
    // SAFETY: the closure runs between fork and exec and only calls
    // prctl and getppid, which are async-signal-safe.
    unsafe {
        cmd.process_group(0).pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                return Err(io::Error::last_os_error());
            }
            // If the engine died between the fork and the prctl, the signal
            // will never come: this process was reparented already.
            if libc::getppid() != parent {
                return Err(io::Error::other(
                    "the engine ended while starting this process",
                ));
            }
            Ok(())
        });
    }
    // The guard is there before the first child, so that no child can exist
    // without one watching.
    guard::ensure_running();
    let child = cmd.spawn()?;
    guard::watch(child.id());
    Ok(Group {
        child,
        reaped: false,
    })
}

impl Group {
    #[cfg(test)]
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Kills every process in the group. Nothing happens once the leader has
    /// been reaped: its id may belong to someone else by then.
    pub fn kill(&mut self) {
        if !self.reaped {
            kill_group(self.child.id());
        }
    }

    /// The exit status, if the leader has ended. When it has, whatever it left
    /// running is killed first, and then it is reaped: while it is unreaped
    /// its id can't be given to another process, so the kill can't hit one.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.reaped {
            return self.child.try_wait();
        }
        if leader_ended(self.child.id(), false)? {
            return self.reap().map(Some);
        }
        Ok(None)
    }

    /// Waits for the leader to end, and then does what `try_wait` does.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if self.reaped {
            return self.child.wait();
        }
        leader_ended(self.child.id(), true)?;
        self.reap()
    }

    fn reap(&mut self) -> io::Result<ExitStatus> {
        kill_group(self.child.id());
        let status = self.child.wait()?;
        self.reaped = true;
        guard::forget(self.child.id());
        Ok(status)
    }
}

/// A pidfd for process `pid`, which becomes readable when the process ends.
/// Only right while `pid` is a child that hasn't been reaped (its id can't be
/// someone else's then). `None` if the kernel can't (older than 5.3).
pub fn pidfd(pid: u32) -> Option<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    // SAFETY: a non-negative result is a new descriptor that nothing else owns.
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
}

/// Waits up to `limit` for `fd` (a pidfd) to become readable: true if it
/// did, which is when its process ended.
pub fn wait_readable(fd: &OwnedFd, limit: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + limit;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        // Rounded up, so a wait that wakes a moment early doesn't spin.
        let ms = left.as_micros().div_ceil(1000).min(i32::MAX as u128) as libc::c_int;
        let mut p = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        match unsafe { libc::poll(&mut p, 1, ms) } {
            1.. => return Ok(true),
            0 => return Ok(false),
            _ => {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            }
        }
    }
}

pub fn kill_group(pgid: u32) {
    // An error means the group is gone already.
    unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) };
}

/// Whether the process has ended, without reaping it (`WNOWAIT`): a process
/// that ended and wasn't reaped is a zombie, which keeps its id. With `block`
/// it waits for that.
fn leader_ended(pid: u32, block: bool) -> io::Result<bool> {
    let flags = libc::WEXITED | libc::WNOWAIT | if block { 0 } else { libc::WNOHANG };
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) };
        if rc == 0 {
            // With WNOHANG, `si_pid` stays 0 when there was nothing to report.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// The watcher that kills what the engine would have killed, if the engine is
/// killed too hard to do it itself.
///
/// The engine starts it (the same binary, `qveil-core __guard`) before its
/// first child and tells it the group of each child over a pipe. The guard
/// does nothing until the pipe closes, which happens when the engine ends, by
/// any means, and then it kills the groups that are still listed.
pub mod guard {
    use super::kill_group;
    use crate::util::lock;
    use std::collections::HashSet;
    use std::io::{BufRead, Write};
    use std::process::ChildStdin;
    use std::sync::Mutex;

    /// The argument that makes the binary run as the guard (see `main`).
    pub const ARG: &str = "__guard";

    enum State {
        NotStarted,
        Running(ChildStdin),
        /// It couldn't be started (or died). The engine carries on without
        /// it: the direct children still die with the engine.
        Unavailable,
    }

    static GUARD: Mutex<State> = Mutex::new(State::NotStarted);

    pub fn ensure_running() {
        let mut state = lock(&GUARD);
        if matches!(*state, State::NotStarted) {
            *state = if cfg!(test) {
                // A unit test's binary can't be the guard (it would run the
                // tests again); they exercise `run` directly instead.
                State::Unavailable
            } else {
                match start() {
                    Ok(stdin) => State::Running(stdin),
                    Err(e) => {
                        note!(
                            "qveil: couldn't start the process guard ({e}); children still end with the engine, but their own children might not"
                        );
                        State::Unavailable
                    }
                }
            };
        }
    }

    fn start() -> std::io::Result<ChildStdin> {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        // `/proc/self/exe` rather than `current_exe`, which fails with
        // " (deleted)" when a package upgrade replaced the binary under us.
        let mut child = Command::new("/proc/self/exe")
            .arg(ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Its own group, so a Ctrl+C aimed at the engine's doesn't take
            // the guard down before the engine has finished cleaning up.
            .process_group(0)
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no pipe to the guard"))
    }

    fn send(line: &str) {
        let mut state = lock(&GUARD);
        if let State::Running(stdin) = &mut *state
            && stdin.write_all(line.as_bytes()).is_err()
        {
            *state = State::Unavailable;
        }
    }

    pub fn watch(pgid: u32) {
        send(&format!("+{pgid}\n"));
    }

    pub fn forget(pgid: u32) {
        send(&format!("-{pgid}\n"));
    }

    /// The guard's own loop: reads `+<group>` and `-<group>` lines until the
    /// input ends, then kills the groups still listed.
    pub fn run(input: impl BufRead) {
        let mut groups = HashSet::new();
        for line in input.lines() {
            let Ok(line) = line else { break };
            let (sign, id) = line.split_at_checked(1).unwrap_or_default();
            let Ok(id) = id.parse::<u32>() else { continue };
            match sign {
                "+" => groups.insert(id),
                "-" => groups.remove(&id),
                _ => false,
            };
        }
        for id in groups {
            kill_group(id);
        }
    }

    /// What `qveil-core __guard` does. Never returns an error: there is nobody to tell.
    pub fn main() {
        // Terminal and `kill` signals aimed at the engine's group are not
        // for the guard; it ends when the engine does, and not before.
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGPIPE] {
            unsafe { libc::signal(sig, libc::SIG_IGN) };
        }
        run(std::io::stdin().lock());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{TempDir, script};
    use std::path::Path;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    fn gone(pid: &str) -> bool {
        // A zombie that nobody reaped is as gone as a process can be.
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| s.rsplit(") ").next().is_some_and(|r| r.starts_with('Z')))
            .unwrap_or(true)
    }

    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let started = Instant::now();
        while !cond() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "gave up on {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A script that starts a grandchild, records both process ids and waits.
    fn with_grandchild(dir: &TempDir) -> String {
        script(
            &dir.0.join("tree"),
            r#"d=$(dirname "$0")
sleep 60 &
echo $! > "$d/grandchild.tmp"; mv "$d/grandchild.tmp" "$d/grandchild"
echo $$ > "$d/leader.tmp"; mv "$d/leader.tmp" "$d/leader"
wait
"#,
        )
    }

    fn pids(dir: &TempDir) -> (String, String) {
        let read = |name: &str| std::fs::read_to_string(dir.0.join(name)).ok();
        let mut found = None;
        wait_until("the tree to start", || {
            found = read("leader").zip(read("grandchild"));
            found.is_some()
        });
        let (a, b) = found.unwrap();
        (a.trim().into(), b.trim().into())
    }

    #[test]
    fn a_child_leads_a_group_of_its_own_and_killing_it_takes_the_grandchild_too() {
        let dir = TempDir::new();
        let mut group = spawn(Command::new(with_grandchild(&dir)).stdin(Stdio::null())).unwrap();
        let (leader, grandchild) = pids(&dir);
        assert_eq!(leader, group.id().to_string());
        // Its group is its own, not the engine's.
        let pgid = unsafe { libc::getpgid(group.id() as libc::pid_t) };
        assert_eq!(pgid as u32, group.id());
        assert_ne!(pgid, unsafe { libc::getpgrp() });

        group.kill();
        group.wait().unwrap();
        wait_until("the grandchild to die", || gone(&grandchild));
    }

    #[test]
    fn what_a_child_leaves_running_is_killed_when_it_ends() {
        let dir = TempDir::new();
        // The leader exits at once and leaves the sleeper behind.
        let bin = script(
            &dir.0.join("leaves"),
            "d=$(dirname \"$0\")\nsleep 60 &\necho $! > \"$d/pid.tmp\"; mv \"$d/pid.tmp\" \"$d/pid\"\nexit 0\n",
        );
        let mut group = spawn(Command::new(bin).stdin(Stdio::null())).unwrap();
        group.wait().unwrap();
        let pid = std::fs::read_to_string(dir.0.join("pid")).unwrap();
        wait_until("the leftover to die", || gone(pid.trim()));
    }

    #[test]
    fn the_guard_kills_the_listed_groups_when_its_input_ends() {
        let dir = TempDir::new();
        let mut group = spawn(Command::new(with_grandchild(&dir)).stdin(Stdio::null())).unwrap();
        let other = spawn(Command::new("sleep").arg("60").stdin(Stdio::null())).unwrap();
        let (leader, grandchild) = pids(&dir);
        // One is listed and forgotten again (it was reaped, say), one stays.
        let input = format!(
            "+{}\n+{}\n-{}\njunk\n+x\n",
            group.id(),
            other.id(),
            other.id()
        );
        guard::run(std::io::Cursor::new(input));
        wait_until("the listed group to die", || {
            gone(&leader) && gone(&grandchild)
        });
        group.wait().unwrap();
        // The forgotten one is left alone.
        assert!(!gone(&other.id().to_string()));
        let mut other = other;
        other.kill();
        other.wait().unwrap();
    }

    #[test]
    fn a_reaped_group_is_not_signalled_again() {
        let mut group = spawn(&mut Command::new("true")).unwrap();
        group.wait().unwrap();
        // Would be a signal to whoever has the id now.
        group.kill();
        assert!(group.try_wait().unwrap().is_some());
        assert!(Path::new("/proc").exists());
    }
}
