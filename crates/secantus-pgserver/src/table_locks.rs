//! `LOCK TABLE`: explicit table locks, process-wide, held until the
//! transaction that took them ends.
//!
//! The eight modes and their conflict table are PostgreSQL's (lock.c's
//! `LockConflicts`); a session never conflicts with itself. An ordinary
//! statement does not TAKE a hold here, but it waits while another session
//! holds a mode that conflicts with the one PostgreSQL would give it --
//! ACCESS SHARE for a read, ROW EXCLUSIVE for a write -- so a table locked
//! `IN ACCESS EXCLUSIVE MODE` shuts out other sessions' readers and writers
//! until the locking transaction ends, as it does in PostgreSQL. What that
//! leaves out: a `LOCK` does not wait for another session's IN-FLIGHT
//! statement, which took no hold.

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
    released: Condvar,
    /// `holds.len()`, readable without the lock: the common case -- no
    /// `LOCK TABLE` anywhere -- costs every statement one atomic load.
    count: AtomicUsize,
}

fn table() -> &'static Table {
    static TABLE: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| Table {
        holds: Mutex::new(Vec::new()),
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

/// Is any table locked at all?
pub fn any() -> bool {
    table().count.load(Ordering::Relaxed) > 0
}

/// Wait until `mode` on `table` would not conflict with another session's
/// hold, then (when `take`) take it. With `nowait` a conflict is `Ok(false)`
/// at once rather than a wait. `cancelled` is polled while waiting.
pub fn acquire<E>(
    table_name: &str,
    pid: i32,
    mode: i32,
    take: bool,
    nowait: bool,
    mut cancelled: impl FnMut() -> Result<(), E>,
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
        let mut wait = || -> Result<std::sync::MutexGuard<'static, Vec<Hold>>, E> {
            let mut holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
            while conflicts(&holds, table_name, pid, mode) {
                let (guard, _) = t
                    .released
                    .wait_timeout(holds, Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner());
                drop(guard);
                cancelled()?;
                holds = t.holds.lock().unwrap_or_else(|e| e.into_inner());
            }
            Ok(holds)
        };
        holds = if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(wait)?
        } else {
            wait()?
        };
    }
    if take {
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
