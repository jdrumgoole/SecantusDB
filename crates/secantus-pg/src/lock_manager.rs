//! The wait-for graph every lock wait registers in.
//!
//! Table locks (`table_locks`), row waits (`row_waits`) and advisory locks
//! (`advisory`) each know which of their own resources a session waits for,
//! but the cycle that makes a deadlock can run through any of them: a
//! session holding an advisory lock and waiting for a row, blocked by one
//! holding the row and waiting on the advisory lock, is a deadlock no
//! single resource's own graph can see. So every wait registers here, as
//! edges `waiter -> the sessions it waits on`, and one depth-first search
//! answers for all lock kinds.
//!
//! Only REAL holder edges are ever registered — the principle `row_waits`
//! documents for row waits: a waiter waits on the sessions actually holding
//! what it wants, never on a guess. A false-positive 40P01 is a bug, while
//! a cycle that forms after a check is caught by the next one. As in
//! PostgreSQL the check runs once `deadlock_timeout` (1 s) into a wait, and
//! of two sessions closing a cycle it is the one whose check runs first
//! that fails.
//!
//! The RELEASER clears the edges, not only the waiter. PostgreSQL's
//! `LockRelease` grants a waiter its lock (`ProcLockWakeup`) before the
//! holder's COMMIT answers, so once that answer is in, `pg_blocking_pids`
//! no longer names the holder. A waiter here wakes on its own poll, up to
//! 10 ms (more under load) after the holder let go; until it did, its edge
//! to the holder still stood and `pg_isolation_test_session_is_blocked`
//! said it was blocked by a session whose COMMIT had already answered
//! (48 of 50 times, measured 2026-10-27; PostgreSQL 15.19: none; RP-270). So a
//! session ending its transaction removes itself from every wait on the
//! locks it held (`released`), and a waiter reads its holders and records
//! them under the graph's lock (`WaitGuard::set_with`), so a release can
//! never land between the two and leave an edge behind.

use std::sync::Mutex;

/// What kind of lock a wait is for, as `pg_stat_activity.wait_event`
/// names it (`Lock` wait event type).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitKind {
    /// A table lock wait.
    Relation,
    /// A row wait: PostgreSQL reports it as waiting on the holder's
    /// transaction (`transactionid`).
    TransactionId,
    /// An advisory-lock wait.
    Advisory,
}

impl WaitKind {
    /// `pg_stat_activity.wait_event`'s value.
    pub fn wait_event(self) -> &'static str {
        match self {
            WaitKind::Relation => "relation",
            WaitKind::TransactionId => "transactionid",
            WaitKind::Advisory => "advisory",
        }
    }
}

/// One session's wait: `pid` is blocked on every pid in `blockers`.
struct Wait {
    pid: i32,
    kind: WaitKind,
    blockers: Vec<i32>,
}

struct Graph {
    /// The waits in flight; one entry per waiting session.
    waits: Mutex<Vec<Wait>>,
}

fn graph() -> &'static Graph {
    static GRAPH: std::sync::OnceLock<Graph> = std::sync::OnceLock::new();
    GRAPH.get_or_init(|| Graph {
        waits: Mutex::new(Vec::new()),
    })
}

/// Is `pid` in a cycle of the wait-for graph: a session it waits on waiting,
/// directly or not, on `pid`?
fn deadlocked(waits: &[Wait], pid: i32) -> bool {
    let blockers = |p: i32| -> Vec<i32> {
        waits
            .iter()
            .filter(|w| w.pid == p)
            .flat_map(|w| w.blockers.iter().copied())
            .collect()
    };
    let mut seen = vec![pid];
    let mut stack = blockers(pid);
    while let Some(p) = stack.pop() {
        if p == pid {
            return true;
        }
        if !seen.contains(&p) {
            seen.push(p);
            stack.extend(blockers(p));
        }
    }
    false
}

/// A registered wait; dropping it ends the wait. One guard per blocked
/// acquisition; `set` replaces the session's edges wholesale as the holders
/// it waits on commit and roll back.
pub struct WaitGuard {
    pid: i32,
    kind: WaitKind,
}

impl WaitGuard {
    pub fn new(pid: i32, kind: WaitKind) -> Self {
        Self { pid, kind }
    }

    /// Record whom `pid` now waits on. An empty `blockers` clears the wait.
    /// A session's own pid is dropped: a session never waits on itself (the
    /// resource tables exclude a session's own holds), and a self-edge is
    /// not a deadlock.
    pub fn set(&self, blockers: Vec<i32>) {
        let mut waits = graph().waits.lock().unwrap_or_else(|e| e.into_inner());
        self.record(&mut waits, blockers);
    }

    /// Read whom `pid` waits on with `holders` and record it, both under
    /// the graph's lock: a holder that ends its transaction (`released`)
    /// meanwhile is either read as gone or has its edge removed after this
    /// records it, never left standing. Returns what `holders` answered.
    /// `holders` must not take the graph's lock.
    pub fn set_with(&self, holders: impl FnOnce() -> Vec<i32>) -> Vec<i32> {
        let mut waits = graph().waits.lock().unwrap_or_else(|e| e.into_inner());
        let blockers = holders();
        self.record(&mut waits, blockers.clone());
        blockers
    }

    fn record(&self, waits: &mut Vec<Wait>, blockers: Vec<i32>) {
        let mut blockers = blockers;
        blockers.retain(|b| *b != self.pid);
        blockers.sort_unstable();
        blockers.dedup();
        waits.retain(|w| w.pid != self.pid);
        if !blockers.is_empty() {
            waits.push(Wait {
                pid: self.pid,
                kind: self.kind,
                blockers,
            });
        }
    }

    /// Does this wait close a cycle? See the module comment: only real
    /// holder edges are registered, so a `true` here is a real deadlock.
    pub fn deadlocked(&self) -> bool {
        let waits = graph().waits.lock().unwrap_or_else(|e| e.into_inner());
        deadlocked(&waits, self.pid)
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        graph()
            .waits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|w| w.pid != self.pid);
    }
}

/// `pid` let go of every lock of `kind` it held (its transaction ended):
/// no session waits on it for one any more. Called after the lock's own
/// table has dropped `pid`'s holds and before the session answers, as
/// PostgreSQL's releaser grants its waiters (see the module comment). A
/// wait left with no blocker is over: its session is running again, and
/// re-registers if it finds another holder.
pub fn released(pid: i32, kind: WaitKind) {
    let mut waits = graph().waits.lock().unwrap_or_else(|e| e.into_inner());
    for w in waits.iter_mut().filter(|w| w.kind == kind) {
        w.blockers.retain(|b| *b != pid);
    }
    waits.retain(|w| !w.blockers.is_empty());
}

/// The sessions `pid` is currently known to wait on, for
/// `pg_blocking_pids(pid)`.
pub fn blockers_of(pid: i32) -> Vec<i32> {
    let waits = graph().waits.lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<i32> = waits
        .iter()
        .filter(|w| w.pid == pid)
        .flat_map(|w| w.blockers.iter().copied())
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// What `pid` waits on right now, for `pg_stat_activity.wait_event`
/// (`wait_event_type` is `Lock` whenever this is `Some`).
pub fn wait_kind(pid: i32) -> Option<WaitKind> {
    let waits = graph().waits.lock().unwrap_or_else(|e| e.into_inner());
    waits.iter().find(|w| w.pid == pid).map(|w| w.kind)
}

/// Every wait in flight: `(waiting session, its kind, the sessions it
/// waits on)`, for `pg_locks`' ungranted rows.
pub fn waits() -> Vec<(i32, WaitKind, Vec<i32>)> {
    graph()
        .waits
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|w| (w.pid, w.kind, w.blockers.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(pid: i32, blockers: &[i32]) -> Wait {
        Wait {
            pid,
            kind: WaitKind::Relation,
            blockers: blockers.to_vec(),
        }
    }

    #[test]
    fn a_direct_cycle_is_a_deadlock() {
        let waits = [wait(1, &[2]), wait(2, &[1])];
        assert!(deadlocked(&waits, 1));
        assert!(deadlocked(&waits, 2));
    }

    #[test]
    fn a_longer_cycle_is_a_deadlock() {
        let waits = [wait(1, &[2]), wait(2, &[3]), wait(3, &[4]), wait(4, &[1])];
        assert!(deadlocked(&waits, 3));
    }

    #[test]
    fn a_chain_that_does_not_close_is_not_a_deadlock() {
        let waits = [wait(1, &[2]), wait(2, &[3])];
        assert!(!deadlocked(&waits, 1));
        assert!(!deadlocked(&waits, 2));
    }

    #[test]
    fn waiting_on_nobody_is_not_a_deadlock() {
        let waits = [wait(1, &[]), wait(2, &[1])];
        assert!(!deadlocked(&waits, 1));
    }

    #[test]
    fn a_session_never_waits_on_itself() {
        // A self-edge is dropped on registration: alone it is no wait at all,
        // and mixed with a real blocker it does not read as a cycle.
        let guard = WaitGuard::new(9_003_001, WaitKind::Relation);
        guard.set(vec![9_003_001]);
        assert!(!guard.deadlocked());
        guard.set(vec![9_003_001, 9_003_002]);
        assert!(!guard.deadlocked(), "waiting on 2 alone closes no cycle");
        drop(guard);
    }

    #[test]
    fn guard_edges_are_replaced_and_dropped() {
        let a = WaitGuard::new(9_004_001, WaitKind::Relation);
        let b = WaitGuard::new(9_004_002, WaitKind::Relation);
        let c = WaitGuard::new(9_004_003, WaitKind::Relation);
        // A waits on B; B waits on C: a chain, no cycle.
        a.set(vec![9_004_002]);
        b.set(vec![9_004_003]);
        assert!(!a.deadlocked());
        // A's edges are replaced wholesale, and now close a cycle with B.
        a.set(vec![9_004_002, 9_004_003]);
        b.set(vec![9_004_001]);
        assert!(a.deadlocked());
        assert!(b.deadlocked());
        // C still waits on nobody.
        assert!(!c.deadlocked());
        drop(a);
        assert!(!b.deadlocked(), "the cycle broke when A's wait ended");
        drop(b);
        drop(c);
    }

    #[test]
    fn a_release_ends_the_waits_on_the_releaser() {
        // A waits on B and C for rows, D on B for a table: B's transaction
        // ends, and with it every row wait's edge to B -- not the table
        // wait's, a lock B's row release did not let go of.
        let a = WaitGuard::new(9_005_001, WaitKind::TransactionId);
        let d = WaitGuard::new(9_005_004, WaitKind::Relation);
        a.set(vec![9_005_002, 9_005_003]);
        d.set(vec![9_005_002]);
        released(9_005_002, WaitKind::TransactionId);
        assert_eq!(blockers_of(9_005_001), vec![9_005_003]);
        assert_eq!(blockers_of(9_005_004), vec![9_005_002]);
        // C's ends too: A waits on nobody, so it is not waiting.
        released(9_005_003, WaitKind::TransactionId);
        assert_eq!(blockers_of(9_005_001), Vec::<i32>::new());
        assert_eq!(wait_kind(9_005_001), None);
        released(9_005_002, WaitKind::Relation);
        assert_eq!(wait_kind(9_005_004), None);
        drop(a);
        drop(d);
    }

    #[test]
    fn set_with_reads_the_holders_under_the_graphs_lock() {
        let a = WaitGuard::new(9_006_001, WaitKind::TransactionId);
        let got = a.set_with(|| vec![9_006_002]);
        assert_eq!(got, vec![9_006_002]);
        assert_eq!(blockers_of(9_006_001), vec![9_006_002]);
        assert_eq!(a.set_with(Vec::new), Vec::<i32>::new());
        assert_eq!(wait_kind(9_006_001), None);
        drop(a);
    }

    #[test]
    fn a_registered_cycle_deadlocks() {
        let a = WaitGuard::new(9_002_001, WaitKind::Relation);
        let b = WaitGuard::new(9_002_002, WaitKind::Relation);
        a.set(vec![9_002_002]);
        b.set(vec![9_002_001]);
        assert!(a.deadlocked());
        drop(b);
        assert!(!a.deadlocked(), "the cycle broke when B's wait ended");
        drop(a);
    }
}
