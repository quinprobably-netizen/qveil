//! One request at a time for the same thing.
//!
//! Two requests for the same comments, or two refreshes of the same lens,
//! that arrive together would each ask YouTube for it: twice the requests
//! for exactly the same answer. With a `Flights`, the second waits for the
//! first to be done, and then looks again at what the first left behind. A
//! first one that ended without an answer (its client hung up, say) leaves
//! the second to ask for itself.

use crate::util::lock;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How often a request that waits its turn looks whether its own client is
/// still there.
const POLL: Duration = Duration::from_millis(250);

pub(super) struct Flights<K> {
    /// What is being fetched, by what it is. An entry is there while a
    /// request has its turn or waits for one.
    flying: Mutex<HashMap<K, Arc<Flight>>>,
}

#[derive(Default)]
struct Flight {
    busy: Mutex<bool>,
    done: Condvar,
}

/// A request's turn for `key`: no other request for it has one until this
/// is dropped.
pub(super) struct Turn<'a, K: Eq + Hash + Clone> {
    flights: &'a Flights<K>,
    key: K,
    flight: Arc<Flight>,
    /// Whether another request had the turn when this one asked for it.
    pub waited: bool,
}

impl<K> Default for Flights<K> {
    fn default() -> Self {
        Flights {
            flying: Mutex::default(),
        }
    }
}

impl<K: Eq + Hash + Clone> Flights<K> {
    /// Takes the turn for `key`, once no other request has it. `None` once
    /// `gone` says that nobody waits for this one any more.
    pub(super) fn take(&self, key: &K, gone: &dyn Fn() -> bool) -> Option<Turn<'_, K>> {
        let flight = Arc::clone(lock(&self.flying).entry(key.clone()).or_default());
        let mut busy = lock(&flight.busy);
        let waited = *busy;
        while *busy {
            if gone() {
                drop(busy);
                self.leave(key, &flight);
                return None;
            }
            busy = flight
                .done
                .wait_timeout(busy, POLL)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *busy = true;
        drop(busy);
        Some(Turn {
            flights: self,
            key: key.clone(),
            flight,
            waited,
        })
    }

    /// Forgets `flight` if nobody has it or waits for it any more: the map's
    /// own reference and the caller's are all there is.
    fn leave(&self, key: &K, flight: &Arc<Flight>) {
        let mut flying = lock(&self.flying);
        if Arc::strong_count(flight) == 2 {
            flying.remove(key);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        lock(&self.flying).len()
    }
}

impl<K: Eq + Hash + Clone> Drop for Turn<'_, K> {
    fn drop(&mut self) {
        *lock(&self.flight.busy) = false;
        self.flight.done.notify_all();
        self.flights.leave(&self.key, &self.flight);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn a_second_request_for_the_same_thing_waits_for_the_first() {
        let flights: Flights<&str> = Flights::default();
        let first = flights.take(&"a", &|| false).unwrap();
        assert!(!first.waited);
        // Another key has a turn of its own at once.
        let other = flights.take(&"b", &|| false).unwrap();
        assert!(!other.waited);
        drop(other);
        let (inside, done) = (AtomicUsize::new(0), AtomicBool::new(false));
        std::thread::scope(|s| {
            s.spawn(|| {
                let second = flights.take(&"a", &|| false).unwrap();
                assert!(second.waited);
                assert!(done.load(Ordering::SeqCst), "it had its turn first");
                inside.fetch_add(1, Ordering::SeqCst);
            });
            // The waiter is there: the entry is shared, and it hasn't got in.
            while Arc::strong_count(&lock(&flights.flying)["a"]) < 3 {
                std::thread::yield_now();
            }
            assert_eq!(inside.load(Ordering::SeqCst), 0);
            done.store(true, Ordering::SeqCst);
            drop(first);
        });
        assert_eq!(inside.load(Ordering::SeqCst), 1);
        assert_eq!(flights.len(), 0, "nothing is left behind");
    }

    #[test]
    fn a_request_whose_client_is_gone_stops_waiting() {
        let flights: Flights<&str> = Flights::default();
        let first = flights.take(&"a", &|| false).unwrap();
        assert!(flights.take(&"a", &|| true).is_none());
        drop(first);
        assert_eq!(flights.len(), 0);
        // Nobody had it: there is nothing to wait for, gone or not.
        assert!(flights.take(&"a", &|| true).is_some());
    }
}
