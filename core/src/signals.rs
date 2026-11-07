//! Cleaning up when qveil is asked to stop.
//!
//! SIGINT (Ctrl+C), SIGTERM and SIGHUP kill a process before it can remove
//! anything, and the yt-dlp processes it started would carry on with the
//! proxy's password in their config files. A signal handler may do almost
//! nothing safely, so the handler only writes a byte to a pipe, and a thread
//! that reads the pipe does the real work, then dies of the signal as it would have.

use std::io::Read;
use std::os::fd::IntoRawFd;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

/// The write end of the pipe the handler uses; -1 until `on_stop` has run.
static WAKE: AtomicI32 = AtomicI32::new(-1);

/// How long the cleanup may take before the process ends anyway, so that a
/// stuck lock can't make Ctrl+C do nothing.
const CLEANUP_LIMIT: Duration = Duration::from_secs(5);

const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

extern "C" fn handler(sig: libc::c_int) {
    // Only async-signal-safe calls: an atomic load and write(2).
    let fd = WAKE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        unsafe { libc::write(fd, (&raw const byte).cast(), 1) };
    }
}

/// Runs `cleanup` when SIGINT, SIGTERM or SIGHUP arrives, and then ends the
/// process by that same signal, so its parent sees what it would have seen.
pub fn on_stop(cleanup: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    let (mut wake, notify) = std::io::pipe()?;
    // Never closed: the handler may run at any time until the process ends.
    let notify = notify.into_raw_fd();
    // A full pipe must not block the handler.
    if unsafe { libc::fcntl(notify, libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    WAKE.store(notify, Ordering::Relaxed);
    for sig in SIGNALS {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(sig, &action, std::ptr::null_mut()) == -1 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            let mut byte = [0u8; 1];
            let sig = loop {
                match wake.read(&mut byte) {
                    Ok(1) => break libc::c_int::from(byte[0]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    _ => return,
                }
            };
            // On its own thread, so a hang is cut off at the limit.
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                cleanup();
                let _ = done.send(());
            });
            let _ = finished.recv_timeout(CLEANUP_LIMIT);
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
            // If raising it didn't end the process (it was blocked), say so by the usual status.
            std::process::exit(128 + sig);
        })?;
    Ok(())
}
