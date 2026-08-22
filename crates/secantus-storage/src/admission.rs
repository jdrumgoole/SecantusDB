//! Admission control for storage-engine writes.
//!
//! Under sustained write load WiredTiger's cache fills with dirty content
//! faster than eviction can clear it. Once it crosses the eviction trigger,
//! application threads are conscripted into eviction work — and because every
//! connection thread dives straight into the engine, they are conscripted *all
//! at once*. That is the p99.9 convoy measured in `tasks/backlog.md`: the
//! median stays flat while the tail explodes, every worker stalling in the
//! same millisecond.
//!
//! The fix is not to evict harder (measured: every WiredTiger eviction knob
//! either did nothing or made the tail worse). It is to stop over-committing
//! the engine in the first place. This is what mongod's ticket system does:
//! bound the number of writes inside the engine and make the rest queue
//! *outside* it, where waiting is cheap and orderly.
//!
//! The measured trade-off on this workload: going from 16 concurrent writers
//! to 4 cost 23% of throughput and halved the tail; going to 2 cost 46% for a
//! 93% tail reduction.
//!
//! Disabled by default (`limit == 0`), so a server that does not opt in
//! behaves exactly as before — the acquire path is a single comparison.

use std::cell::Cell;
use std::sync::{Condvar, Mutex};

thread_local! {
    /// Re-entrancy guard.
    ///
    /// A multi-document transaction calls several write methods while already
    /// admitted. Without this, a thread holding the last ticket would queue
    /// behind itself the moment it made a nested write call, and the server
    /// would deadlock rather than merely slow down. Admission is therefore
    /// per-thread-entry, not per-call: the outermost write holds the ticket
    /// and every nested one rides along on it.
    static HOLDING: Cell<bool> = const { Cell::new(false) };
}

/// A bounded pool of permits to be inside the storage engine's write path.
#[derive(Debug)]
pub struct Tickets {
    /// 0 means unlimited — admission control off.
    limit: usize,
    in_flight: Mutex<usize>,
    space: Condvar,
}

/// A held permit. Releases on drop, including while unwinding from a panic:
/// a write that panics must not leak a ticket, or the pool bleeds down to
/// zero and the server wedges.
#[derive(Debug)]
pub struct Ticket<'a> {
    pool: Option<&'a Tickets>,
}

impl Tickets {
    pub fn new(limit: usize) -> Tickets {
        Tickets {
            limit,
            in_flight: Mutex::new(0),
            space: Condvar::new(),
        }
    }

    /// 0 when admission control is disabled.
    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn enabled(&self) -> bool {
        self.limit > 0
    }

    /// Writes currently admitted. Diagnostics only.
    pub fn in_flight(&self) -> usize {
        *self.in_flight.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Block until a permit is free, then take it.
    ///
    /// Returns a no-op ticket when admission control is off, or when this
    /// thread is already admitted (see [`HOLDING`]).
    pub fn acquire(&self) -> Ticket<'_> {
        if self.limit == 0 || HOLDING.with(|h| h.get()) {
            return Ticket { pool: None };
        }
        let mut n = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        while *n >= self.limit {
            n = self.space.wait(n).unwrap_or_else(|e| e.into_inner());
        }
        *n += 1;
        HOLDING.with(|h| h.set(true));
        Ticket { pool: Some(self) }
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        let Some(pool) = self.pool else { return };
        {
            let mut n = pool.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            *n = n.saturating_sub(1);
        }
        HOLDING.with(|h| h.set(false));
        pool.space.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn a_zero_limit_admits_everyone() {
        let t = Tickets::new(0);
        assert!(!t.enabled());
        let _a = t.acquire();
        let _b = t.acquire();
        // No permits are actually taken when disabled.
        assert_eq!(t.in_flight(), 0);
    }

    #[test]
    fn a_ticket_is_returned_on_drop() {
        let t = Tickets::new(2);
        {
            let _a = t.acquire();
            assert_eq!(t.in_flight(), 1);
        }
        assert_eq!(t.in_flight(), 0);
    }

    #[test]
    fn concurrency_never_exceeds_the_limit() {
        const LIMIT: usize = 3;
        let tickets = Arc::new(Tickets::new(LIMIT));
        let now = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let (tickets, now, peak) = (tickets.clone(), now.clone(), peak.clone());
            handles.push(std::thread::spawn(move || {
                for _ in 0..40 {
                    let _t = tickets.acquire();
                    let cur = now.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(cur, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_micros(200));
                    now.fetch_sub(1, Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(
            peak.load(Ordering::SeqCst) <= LIMIT,
            "peak {}",
            peak.load(Ordering::SeqCst)
        );
        assert_eq!(tickets.in_flight(), 0);
    }

    #[test]
    fn every_waiter_eventually_gets_through() {
        // A pool of one, heavily contended: no thread may be starved.
        let tickets = Arc::new(Tickets::new(1));
        let done = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let (tickets, done) = (tickets.clone(), done.clone());
            handles.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    let _t = tickets.acquire();
                    done.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(done.load(Ordering::SeqCst), 200);
    }

    #[test]
    fn a_nested_acquire_does_not_deadlock_against_itself() {
        // The whole pool is one ticket; a nested write on the same thread must
        // ride the outer one rather than wait for it to be released.
        let tickets = Tickets::new(1);
        let outer = tickets.acquire();
        assert_eq!(tickets.in_flight(), 1);
        let inner = tickets.acquire(); // would block forever without re-entrancy
        assert_eq!(
            tickets.in_flight(),
            1,
            "nested acquire must not take a second ticket"
        );
        drop(inner);
        // Dropping the nested guard must not release the outer thread's ticket.
        assert_eq!(tickets.in_flight(), 1);
        drop(outer);
        assert_eq!(tickets.in_flight(), 0);
    }

    #[test]
    fn a_panicking_write_does_not_leak_its_ticket() {
        let tickets = Tickets::new(1);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _t = tickets.acquire();
            panic!("write blew up");
        }));
        assert!(r.is_err());
        assert_eq!(
            tickets.in_flight(),
            0,
            "a leaked ticket would wedge the pool"
        );
        // The pool is still usable.
        let _t = tickets.acquire();
        assert_eq!(tickets.in_flight(), 1);
    }
}
