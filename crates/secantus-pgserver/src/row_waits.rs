//! Sessions waiting for another transaction's row: a READ COMMITTED block
//! whose write collided with a row another transaction holds waits for the
//! writers that may hold it (see `rerun_after_conflict` in `lib.rs`), and a
//! wait that closes a cycle is PostgreSQL's 40P01.
//!
//! WiredTiger does not say which transaction holds a row, so a waiter waits on
//! every session holding a write lock (ROW EXCLUSIVE or stronger) on any table
//! -- a superset of PostgreSQL's single blocker. As in PostgreSQL the cycle is
//! checked ONCE, `deadlock_timeout` (1s) after the wait began, so of two
//! sessions closing a cycle it is the one whose timer fires first that fails.

use std::sync::Mutex;

/// `(waiting session, the sessions it waits on)`.
type Waits = Mutex<Vec<(i32, Vec<i32>)>>;

fn registry() -> &'static Waits {
    static WAITS: std::sync::OnceLock<Waits> = std::sync::OnceLock::new();
    WAITS.get_or_init(|| Mutex::new(Vec::new()))
}

/// The sessions other than `pid` holding a write lock on some table.
pub fn writers_other_than(pid: i32) -> Vec<i32> {
    let mut out: Vec<i32> = crate::table_locks::snapshot()
        .into_iter()
        .filter(|(_, p, mode)| *p != pid && *mode >= crate::table_locks::ROW_EXCLUSIVE)
        .map(|(_, p, _)| p)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// A registered wait; dropping it ends the wait.
pub struct Waiting(i32);

impl Waiting {
    pub fn new(pid: i32) -> Self {
        Self(pid)
    }

    /// Record whom `pid` now waits on.
    pub fn set(&self, blockers: Vec<i32>) {
        let mut waits = registry().lock().unwrap_or_else(|e| e.into_inner());
        waits.retain(|(p, _)| *p != self.0);
        waits.push((self.0, blockers));
    }

    /// Does this wait close a cycle: a session it waits on waiting, directly
    /// or not, on it?
    pub fn deadlocked(&self) -> bool {
        let waits = registry().lock().unwrap_or_else(|e| e.into_inner());
        let blockers = |p: i32| -> Vec<i32> {
            waits
                .iter()
                .filter(|(w, _)| *w == p)
                .flat_map(|(_, b)| b.iter().copied())
                .collect()
        };
        let mut seen = vec![self.0];
        let mut frontier = blockers(self.0);
        while let Some(p) = frontier.pop() {
            if p == self.0 {
                return true;
            }
            if seen.contains(&p) {
                continue;
            }
            seen.push(p);
            frontier.extend(blockers(p));
        }
        false
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(p, _)| *p != self.0);
    }
}
