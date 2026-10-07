//! Advisory locks: `pg_advisory_lock` and its family, process-wide.
//!
//! A lock is a key -- one bigint, or a pair of int4s, which PostgreSQL keeps
//! in separate spaces -- held by sessions. Each acquisition is a hold, so a
//! session that locks twice must unlock twice (PostgreSQL's reentrancy).
//! Exclusive and shared holds conflict across sessions only; a
//! transaction-scoped hold is released when its transaction ends and cannot
//! be unlocked explicitly. Every hold a session has is released when it
//! disconnects.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// A lock key: the argument form (0 bigint, 1 two int4s) and its value.
pub type Key = (u8, i64);

#[derive(Clone, Copy)]
struct Hold {
    key: Key,
    pid: i32,
    shared: bool,
    xact: bool,
}

struct Table {
    holds: Mutex<Vec<Hold>>,
    released: Condvar,
}

fn table() -> &'static Table {
    static TABLE: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| Table {
        holds: Mutex::new(Vec::new()),
        released: Condvar::new(),
    })
}

/// Every session's holds, one per `(key, pid, shared)`, for `pg_locks`.
pub fn snapshot() -> Vec<(Key, i32, bool)> {
    let mut out: Vec<(Key, i32, bool)> = Vec::new();
    for h in table()
        .holds
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
    {
        if !out.contains(&(h.key, h.pid, h.shared)) {
            out.push((h.key, h.pid, h.shared));
        }
    }
    out
}

fn conflicts(holds: &[Hold], key: Key, pid: i32, shared: bool) -> bool {
    holds
        .iter()
        .any(|h| h.key == key && h.pid != pid && (!shared || !h.shared))
}

/// Take a hold, waiting while another session's hold conflicts. `cancelled`
/// is polled while waiting; its error ends the wait.
pub fn lock<E>(
    key: Key,
    pid: i32,
    shared: bool,
    xact: bool,
    cancelled: impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    // Waiting blocks this thread. On a shared runtime worker its queued
    // tasks go to the others first (`blocking_wait`), or a connection
    // scheduled behind this one -- the very one holding the lock -- never
    // runs; on a connection's own thread it blocks only that connection.
    let t = table();
    {
        let holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
        if conflicts(&holds, key, pid, shared) {
            drop(holds);
            return crate::blocking_wait(|| wait_and_take(key, pid, shared, xact, cancelled));
        }
    }
    wait_and_take(key, pid, shared, xact, cancelled)
}

fn wait_and_take<E>(
    key: Key,
    pid: i32,
    shared: bool,
    xact: bool,
    mut cancelled: impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let t = table();
    let mut holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
    while conflicts(&holds, key, pid, shared) {
        let (guard, _) = t
            .released
            .wait_timeout(holds, Duration::from_millis(50))
            .unwrap_or_else(|e| e.into_inner());
        holds = guard;
        drop(holds);
        cancelled()?;
        holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
    }
    holds.push(Hold {
        key,
        pid,
        shared,
        xact,
    });
    Ok(())
}

/// Take a hold if nothing conflicts; `false` otherwise.
pub fn try_lock(key: Key, pid: i32, shared: bool, xact: bool) -> bool {
    let mut holds = table().holds.lock().unwrap_or_else(|e| e.into_inner());
    if conflicts(&holds, key, pid, shared) {
        return false;
    }
    holds.push(Hold {
        key,
        pid,
        shared,
        xact,
    });
    true
}

/// Wait until a hold could be taken, without taking it: a transaction-scoped
/// lock requested outside a transaction block lasts only its statement.
pub fn wait_free<E>(
    key: Key,
    pid: i32,
    shared: bool,
    cancelled: impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    lock(key, pid, shared, true, cancelled)?;
    release_xact(pid);
    Ok(())
}

/// Release one session-level hold; `false` when the session has none.
pub fn unlock(key: Key, pid: i32, shared: bool) -> bool {
    let t = table();
    let mut holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
    let Some(i) = holds
        .iter()
        .position(|h| h.key == key && h.pid == pid && h.shared == shared && !h.xact)
    else {
        return false;
    };
    holds.remove(i);
    t.released.notify_all();
    true
}

fn release(pid: i32, which: impl Fn(&Hold) -> bool) {
    let t = table();
    let mut holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
    let before = holds.len();
    holds.retain(|h| !(h.pid == pid && which(h)));
    if holds.len() != before {
        t.released.notify_all();
    }
}

/// `pg_advisory_unlock_all()`: every session-level hold.
pub fn unlock_all(pid: i32) {
    release(pid, |h| !h.xact);
}

/// At the end of a transaction: its transaction-scoped holds.
pub fn release_xact(pid: i32) {
    release(pid, |h| h.xact);
}

/// At disconnect: everything.
pub fn release_session(pid: i32) {
    release(pid, |_| true);
}
