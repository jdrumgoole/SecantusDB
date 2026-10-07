//! Table locks, process-wide, held until the transaction that took them
//! ends: `LOCK TABLE`'s, and the ones an ordinary statement in a
//! transaction block takes -- ACCESS SHARE for a read, ROW EXCLUSIVE for a
//! write -- so a `LOCK ... IN ACCESS EXCLUSIVE MODE` waits for the
//! transactions that have read or written the table, as in PostgreSQL.
//!
//! The eight modes and their conflict table are PostgreSQL's (lock.c's
//! `LockConflicts`); a session never conflicts with itself. A wait that
//! closes a cycle of sessions waiting on each other is PostgreSQL's 40P01.
//! An autocommit statement takes no hold (it waits on the others' holds),
//! so a `LOCK` does not wait for one that is IN FLIGHT.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

pub const ACCESS_SHARE: i32 = 1;
pub const ROW_EXCLUSIVE: i32 = 3;
pub const ACCESS_EXCLUSIVE: i32 = 8;

struct Hold {
    table: String,
    pid: i32,
    mode: i32,
}

struct Table {
    holds: Mutex<Vec<Hold>>,
    /// The sessions waiting, and on what: `(pid, table, mode)`.
    waiting: Mutex<Vec<(i32, String, i32)>>,
    released: Condvar,
    /// `holds.len()`, readable without the lock: the common case -- no
    /// `LOCK TABLE` anywhere -- costs every statement one atomic load.
    count: AtomicUsize,
}

fn table() -> &'static Table {
    static TABLE: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| Table {
        holds: Mutex::new(Vec::new()),
        waiting: Mutex::new(Vec::new()),
        released: Condvar::new(),
        count: AtomicUsize::new(0),
    })
}

/// Does lock mode `a` conflict with mode `b`?
fn modes_conflict(a: i32, b: i32) -> bool {
    // Row `m` lists the modes mode `m` conflicts with.
    const CONFLICTS: [&[i32]; 9] = [
        &[],
        &[8],
        &[7, 8],
        &[5, 6, 7, 8],
        &[4, 5, 6, 7, 8],
        &[3, 4, 6, 7, 8],
        &[3, 4, 5, 6, 7, 8],
        &[2, 3, 4, 5, 6, 7, 8],
        &[1, 2, 3, 4, 5, 6, 7, 8],
    ];
    CONFLICTS
        .get(a as usize)
        .is_some_and(|row| row.contains(&b))
}

fn conflicts(holds: &[Hold], table: &str, pid: i32, mode: i32) -> bool {
    holds
        .iter()
        .any(|h| h.table == table && h.pid != pid && modes_conflict(mode, h.mode))
}

/// Does waiting for `mode` on `table` close a cycle -- a session this one
/// waits on waiting, directly or not, on this one?
fn deadlocked(holds: &[Hold], waiting: &[(i32, String, i32)], pid: i32) -> bool {
    let blockers = |p: i32| -> Vec<i32> {
        waiting
            .iter()
            .filter(|(w, _, _)| *w == p)
            .flat_map(|(_, t, m)| {
                holds
                    .iter()
                    .filter(|h| &h.table == t && h.pid != p && modes_conflict(*m, h.mode))
                    .map(|h| h.pid)
                    .collect::<Vec<_>>()
            })
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

/// Every hold: `(table, pid, mode)`, for `pg_locks`.
pub fn snapshot() -> Vec<(String, i32, i32)> {
    table()
        .holds
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|h| (h.table.clone(), h.pid, h.mode))
        .collect()
}

/// Every session waiting: `(table, pid, mode)`.
pub fn waiting() -> Vec<(String, i32, i32)> {
    table()
        .waiting
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(p, t, m)| (t.clone(), *p, *m))
        .collect()
}

/// PostgreSQL's name for a lock mode, as `pg_locks.mode` shows it.
pub fn mode_name(mode: i32) -> &'static str {
    match mode {
        1 => "AccessShareLock",
        2 => "RowShareLock",
        3 => "RowExclusiveLock",
        4 => "ShareUpdateExclusiveLock",
        5 => "ShareLock",
        6 => "ShareRowExclusiveLock",
        7 => "ExclusiveLock",
        _ => "AccessExclusiveLock",
    }
}

/// Is any table locked at all?
pub fn any() -> bool {
    table().count.load(Ordering::Relaxed) > 0
}

/// Wait until `mode` on `table` would not conflict with another session's
/// hold, then (when `take`) take it. With `nowait` a conflict is `Ok(false)`
/// at once rather than a wait. `cancelled` is polled while waiting.
#[allow(clippy::too_many_arguments)]
pub fn acquire<E>(
    table_name: &str,
    pid: i32,
    mode: i32,
    take: bool,
    nowait: bool,
    mut cancelled: impl FnMut() -> Result<(), E>,
    deadlock: impl Fn() -> E,
) -> Result<bool, E> {
    let t = table();
    let mut holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
    if conflicts(&holds, table_name, pid, mode) {
        if nowait {
            return Ok(false);
        }
        drop(holds);
        // Waiting blocks this thread, which may be a runtime worker: hand its
        // queued tasks to the others first, or the connection holding the
        // lock -- scheduled behind this one -- never runs to release it.
        let wait = || -> Result<std::sync::MutexGuard<'static, Vec<Hold>>, E> {
            let unwait = || {
                t.waiting
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|(w, _, _)| *w != pid);
            };
            t.waiting.lock().unwrap_or_else(|e| e.into_inner()).push((
                pid,
                table_name.to_string(),
                mode,
            ));
            let mut holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
            let mut polls = 0u32;
            while conflicts(&holds, table_name, pid, mode) {
                // PostgreSQL looks for a deadlock after `deadlock_timeout`
                // (1s) of waiting.
                if polls == 20 {
                    let waiting = t.waiting.lock().unwrap_or_else(|e| e.into_inner());
                    if deadlocked(&holds, &waiting, pid) {
                        drop(waiting);
                        drop(holds);
                        unwait();
                        return Err(deadlock());
                    }
                }
                polls += 1;
                let (guard, _) = t
                    .released
                    .wait_timeout(holds, Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner());
                drop(guard);
                if let Err(e) = cancelled() {
                    unwait();
                    return Err(e);
                }
                holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
            }
            unwait();
            Ok(holds)
        };
        holds = crate::blocking_wait(wait)?;
    }
    // A hold this session already has at this mode needs no second entry.
    if take
        && !holds
            .iter()
            .any(|h| h.table == table_name && h.pid == pid && h.mode == mode)
    {
        holds.push(Hold {
            table: table_name.to_string(),
            pid,
            mode,
        });
        t.count.store(holds.len(), Ordering::Relaxed);
    }
    Ok(true)
}

/// At the end of a transaction, or at disconnect: every hold `pid` has.
pub fn release(pid: i32) {
    let t = table();
    let mut holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
    let before = holds.len();
    holds.retain(|h| h.pid != pid);
    if holds.len() != before {
        t.count.store(holds.len(), Ordering::Relaxed);
        t.released.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conflict_table_is_postgresqls() {
        // ACCESS SHARE only meets ACCESS EXCLUSIVE.
        assert!(modes_conflict(ACCESS_SHARE, ACCESS_EXCLUSIVE));
        assert!(!modes_conflict(ACCESS_SHARE, 7));
        // ROW EXCLUSIVE (a write) conflicts with SHARE and above, not with
        // itself -- two writers do not block each other at table level.
        assert!(!modes_conflict(ROW_EXCLUSIVE, ROW_EXCLUSIVE));
        assert!(modes_conflict(ROW_EXCLUSIVE, 5));
        // SHARE is self-compatible; SHARE ROW EXCLUSIVE is not.
        assert!(!modes_conflict(5, 5));
        assert!(modes_conflict(6, 6));
        // The table is symmetric.
        for a in 1..=8 {
            for b in 1..=8 {
                assert_eq!(modes_conflict(a, b), modes_conflict(b, a), "{a} vs {b}");
            }
        }
    }
}
