//! Transaction ids, as `txid_current()` / `pg_current_xact_id()` and the
//! current snapshot report them (`secantus_pgplan::snapshots`).
//!
//! An xid is assigned to a session's open transaction the first time one is
//! asked for, from a process-wide counter, and released when the transaction
//! ends. The counter starts above anything an earlier run of the server can
//! have handed out: from the store's cluster time, which is persisted and
//! monotonic, as `seconds << 30 | ordinal << 20`.
//!
//! A snapshot follows PostgreSQL's `GetSnapshotData`: `xmax` is one past the
//! latest completed xid, `xip` the xids other sessions hold that are below
//! it, `xmin` the least of those (or `xmax`). The session's own xid is not
//! in its snapshot.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use secantus_pgplan::snapshots::XidOp;

static NEXT: AtomicU64 = AtomicU64::new(3);

struct State {
    running: HashMap<i32, u64>,
    latest_completed: u64,
}

fn state() -> &'static Mutex<State> {
    static STATE: std::sync::OnceLock<Mutex<State>> = std::sync::OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            running: HashMap::new(),
            latest_completed: NEXT.load(Ordering::SeqCst).saturating_sub(1),
        })
    })
}

/// Start the counter above every xid an earlier run handed out, and install
/// the hook the planner asks through.
pub fn install(storage: &secantus_storage::Storage) {
    if let Ok(ts) = storage.current_cluster_time() {
        let base = (u64::from(ts.time) << 30) | (u64::from(ts.increment & 0x3ff) << 20);
        if base > NEXT.load(Ordering::SeqCst) {
            NEXT.store(base, Ordering::SeqCst);
            let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
            if s.running.is_empty() {
                s.latest_completed = base - 1;
            }
        }
    }
    secantus_pgplan::snapshots::set_xid_hook(answer);
}

fn answer(pid: i32, op: XidOp) -> Option<String> {
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    match op {
        XidOp::Assign => Some(
            s.running
                .entry(pid)
                .or_insert_with(|| NEXT.fetch_add(1, Ordering::SeqCst))
                .to_string(),
        ),
        XidOp::IfAssigned => s.running.get(&pid).map(u64::to_string),
        XidOp::Snapshot => {
            let xmax = s.latest_completed + 1;
            let mut xip: Vec<u64> = s
                .running
                .iter()
                .filter(|(p, x)| **p != pid && **x < xmax)
                .map(|(_, x)| *x)
                .collect();
            xip.sort_unstable();
            let xmin = xip.first().copied().unwrap_or(xmax);
            let list: Vec<String> = xip.iter().map(u64::to_string).collect();
            Some(format!("{xmin}:{xmax}:{}", list.join(",")))
        }
    }
}

/// `pid`'s transaction ended: its xid, if it had one, is complete.
pub fn end(pid: i32) {
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(xid) = s.running.remove(&pid) {
        s.latest_completed = s.latest_completed.max(xid);
    }
}
