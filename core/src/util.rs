//! Small helpers that more than one module needs.

use std::sync::{Mutex, MutexGuard};

/// Locks `m`, ignoring poisoning. A panic on one thread must not make every
/// other thread (and the next request) panic on the lock it left behind: what
/// these locks guard is simple state that is valid whenever it is not being
/// changed.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 128 random bits as 32 hex digits: the server's token, session ids.
pub fn random_hex() -> String {
    format!(
        "{:016x}{:016x}",
        rand::random::<u64>(),
        rand::random::<u64>()
    )
}
