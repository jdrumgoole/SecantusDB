//! Sessions waiting for another transaction's row: a READ COMMITTED block
//! whose write collided with a row another transaction holds waits for the
//! writers that may hold it (see `rerun_after_conflict` in `lib.rs`), and a
//! wait that closes a cycle is PostgreSQL's 40P01.
//!
//! WiredTiger does not say which transaction holds a row, so each block
//! PUBLISHES the document rows it has written (`publish`, after each of its
//! statements and whenever it starts to wait -- the storage records every row
//! a transaction writes, its triggers' and FK cascades' writes included,
//! `UserTransactionHandle::written_rows`), and the row a write conflict was
//! on is the row the colliding statement last wrote
//! (`secantus_storage::last_row_written`). The waiter waits on the session(s)
//! holding THAT row, as PostgreSQL waits on its single blocker, and only those
//! edges enter the deadlock check, so a cycle is reported only when it is
//! real. A row nobody has published (an autocommit statement's, or one a
//! block is writing in the statement still running) gives no edge: the
//! waiter retries on the old schedule without a deadlock verdict. As in
//! PostgreSQL the cycle is checked ONCE, `deadlock_timeout` (1s) after the
//! wait began, so of two sessions closing a cycle it is the one whose timer
//! fires first that fails.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use secantus_storage::WrittenRow;

/// `session -> (how many of its written rows are recorded, the rows)`.
type Holders = Mutex<HashMap<i32, (usize, HashSet<WrittenRow>)>>;

fn holders() -> &'static Holders {
    static HOLDERS: std::sync::OnceLock<Holders> = std::sync::OnceLock::new();
    HOLDERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the rows `pid`'s open transaction holds: `rows` is its whole write
/// set so far, of which only the part not yet recorded is added.
pub fn publish(pid: i32, rows: &[WrittenRow]) {
    let mut map = holders().lock().unwrap_or_else(|e| e.into_inner());
    if rows.is_empty() {
        map.remove(&pid);
        return;
    }
    let (seen, set) = map.entry(pid).or_default();
    if rows.len() < *seen {
        set.clear();
        *seen = 0;
    }
    set.extend(rows[*seen..].iter().cloned());
    *seen = rows.len();
}

/// Replace what `pid` holds with `rows`: its transaction was moved onto a new
/// one (`rebase_user_transaction`), whose write set is the replay's.
pub fn republish(pid: i32, rows: &[WrittenRow]) {
    forget(pid);
    publish(pid, rows);
}

/// What a WAITING session holds: its write set without `target`, the row it
/// collided on. The storage records a row before writing it, so the failed
/// write is in the set -- and a waiter listed as holding the row it waits
/// for would close a false cycle with every other waiter on that row.
pub fn publish_waiting(pid: i32, rows: &[WrittenRow], target: Option<&WrittenRow>) {
    let held: Vec<WrittenRow> = rows
        .iter()
        .filter(|r| Some(*r) != target)
        .cloned()
        .collect();
    republish(pid, &held);
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
        .filter(|(p, (_, rows))| **p != pid && rows.contains(row))
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
