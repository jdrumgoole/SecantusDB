//! Sessions waiting for another transaction's row: a READ COMMITTED block
//! whose write collided with a row another transaction holds waits for the
//! writers that may hold it (see `rerun_after_conflict` in `lib.rs`), and a
//! wait that closes a cycle is PostgreSQL's 40P01.
//!
//! WiredTiger does not say which transaction holds a row, so each session
//! REGISTERS the shared row set of its open transaction (`register`, when a
//! transaction handle is opened). The storage fills that set as each row is
//! written -- inside a statement still running, in an autocommit statement's
//! own transaction, by triggers and FK cascades, and for the `_id` index key
//! an INSERT claims (`secantus_storage::ID_KEY_ROW_SUFFIX`) -- and empties it
//! when the transaction ends. The row a write conflict was on is the row the
//! colliding statement last wrote (`secantus_storage::last_row_written`). The
//! waiter waits on the session(s) holding THAT row, as PostgreSQL waits on
//! its single blocker, and only those edges enter the deadlock check, so a
//! cycle is reported only when it is real. A row no registered session holds
//! (a prepared transaction's, or the MongoDB server's) gives no edge: the
//! waiter retries on the old schedule without a deadlock verdict. As in
//! PostgreSQL the cycle is checked ONCE, `deadlock_timeout` (1s) after the
//! wait began, so of two sessions closing a cycle it is the one whose timer
//! fires first that fails.

use std::collections::HashMap;
use std::sync::Mutex;

use secantus_storage::{HeldRows, WrittenRow};

/// `session -> the row set of its open transaction`.
type Holders = Mutex<HashMap<i32, HeldRows>>;

fn holders() -> &'static Holders {
    static HOLDERS: std::sync::OnceLock<Holders> = std::sync::OnceLock::new();
    HOLDERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `pid`'s open transaction is the one whose rows are `rows`.
pub fn register(pid: i32, rows: HeldRows) {
    holders()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(pid, rows);
}

/// Has `pid`'s open transaction written a row of a collection `pred`
/// accepts? (`false` with no transaction registered.)
pub fn wrote_collection(pid: i32, pred: impl Fn(&str, &str) -> bool) -> bool {
    let held = holders()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&pid)
        .cloned();
    held.is_some_and(|h| {
        h.lock()
            .unwrap_or_else(|e| e.into_inner())
            .collections
            .iter()
            .any(|(db, coll)| pred(db, coll))
    })
}

/// `pid`'s transaction ended: it holds no rows.
pub fn forget(pid: i32) {
    holders()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&pid);
}

/// The sessions other than `pid` whose open transaction holds `row`.
pub fn holders_of(row: &WrittenRow, pid: i32) -> Vec<i32> {
    let map = holders().lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<i32> = map
        .iter()
        .filter(|(p, rows)| {
            **p != pid
                && rows
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .rows
                    .contains(row)
        })
        .map(|(p, _)| *p)
        .collect();
    out.sort_unstable();
    out
}

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
