//! Transaction ids, as `txid_current()` / `pg_current_xact_id()` and the
//! current snapshot report them (`secantus_pgplan::snapshots`), and as
//! `age(xid)` measures from.
//!
//! PostgreSQL's model, kept as PostgreSQL keeps it: one counter for the
//! cluster, an `xid8` of `epoch << 32 | xid`, that runs on across restarts.
//! A store starts where `initdb` leaves a cluster -- everything frozen at
//! xid 722 ([`FROZEN_XID`], `datfrozenxid` / `relfrozenxid`), the next xid
//! 723 -- and every transaction that asks for an xid takes the next one, so
//! `age(datfrozenxid)` grows over the store's life as it does on PostgreSQL
//! (whose autovacuum would freeze and advance it; nothing here does).
//!
//! The counter is persisted the way PostgreSQL's `nextXid` effectively is:
//! in blocks. A block of [`BLOCK`] xids is reserved in the store
//! (`__sql_xid__`) before any of it is handed out, and a restart resumes at
//! the end of the last reservation, so an xid is never handed out twice,
//! though one run's unused remainder is skipped.
//!
//! An xid is assigned to a session's open transaction the first time one is
//! asked for, and released when the transaction ends. A snapshot follows
//! PostgreSQL's `GetSnapshotData`: `xmax` is one past the latest completed
//! xid, `xip` the xids other sessions hold that are below it, `xmin` the
//! least of those (or `xmax`). The session's own xid is not in its
//! snapshot.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use secantus_pgplan::snapshots::XidOp;
use secantus_storage::Storage;

/// `datfrozenxid` after `initdb`, and so here for good.
pub const FROZEN_XID: u32 = 722;

/// How many xids one write to the store reserves.
const BLOCK: u64 = 8192;

/// Where the reservation lives: a collection no catalog names, in the
/// cluster's own database.
const XID_DB: &str = "postgres";
const XID_COLLECTION: &str = "__sql_xid__";

/// One store's counter. Each store is its own cluster, so two servers in one
/// process (the embedded handle, a test) count apart. The store is held
/// WEAKLY: this table must never keep a store open past its server's
/// shutdown, or the close-checkpoint would not run.
struct Cluster {
    storage: Weak<Storage>,
    next: u64,
    /// The first xid the store has NOT reserved.
    reserved: u64,
    latest_completed: u64,
    running: HashMap<i32, u64>,
    /// Each running transaction's command counter: `(current command id,
    /// whether a row write used it)`. PostgreSQL's `currentCommandId`.
    commands: HashMap<i32, (u32, bool)>,
}

/// The clusters by their store's address.
fn clusters() -> &'static Mutex<HashMap<usize, Cluster>> {
    static C: OnceLock<Mutex<HashMap<usize, Cluster>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key(storage: &Arc<Storage>) -> usize {
    Arc::as_ptr(storage) as usize
}

thread_local! {
    /// The store of the session whose statement this thread is planning.
    static CURRENT: Cell<usize> = const { Cell::new(0) };
}

/// The store the statements planned on this thread belong to, set beside
/// the session pid (`snapshots::set_session_pid`).
pub fn set_current(storage: &Arc<Storage>) {
    CURRENT.with(|c| c.set(key(storage)));
}

/// The xid everything is frozen at (`datfrozenxid`, `relfrozenxid`).
pub fn frozen_xid() -> u32 {
    FROZEN_XID
}

/// Record that xids below `upto` may be handed out.
fn reserve(storage: &Storage, upto: u64) -> Result<(), String> {
    let upto = i64::try_from(upto).map_err(|_| "transaction ids are exhausted".to_string())?;
    storage
        .outside_user_transaction(|| {
            storage.update_matching(
                XID_DB,
                XID_COLLECTION,
                &bson::doc! { "_id": "next" },
                &bson::doc! { "$set": { "reserved": upto } },
                false,
                true,
                &[],
                &bson::Document::new(),
                None,
                None,
                false,
            )
        })
        .map(|_| ())
        .map_err(|e| format!("could not reserve transaction ids: {e}"))
}

/// Resume `storage`'s counter where its last reservation ended (or at 723
/// for a store that has none), reserve the first block, and install the
/// hook the planner asks through.
pub fn install(storage: &Arc<Storage>) -> Result<(), String> {
    let found = storage
        .find_matching(XID_DB, XID_COLLECTION, &bson::doc! { "_id": "next" })
        .map_err(|e| format!("could not read the transaction id counter: {e}"))?;
    let next = match found.first() {
        Some(raw) => {
            let doc = secantus_core::bsondoc::from_bytes(raw)
                .map_err(|e| format!("could not decode the transaction id counter: {e}"))?;
            match doc.get("reserved") {
                Some(bson::Bson::Int64(n)) => u64::try_from(*n).unwrap_or(0),
                Some(bson::Bson::Int32(n)) => u64::try_from(*n).unwrap_or(0),
                _ => 0,
            }
            .max(u64::from(FROZEN_XID) + 1)
        }
        None => u64::from(FROZEN_XID) + 1,
    };
    reserve(storage, next + BLOCK)?;
    let mut all = clusters().lock().unwrap_or_else(|e| e.into_inner());
    // A store whose server has gone is dropped from the table; its
    // address may be reused by the next one.
    all.retain(|_, c| c.storage.strong_count() > 0);
    all.insert(
        key(storage),
        Cluster {
            storage: Arc::downgrade(storage),
            next,
            reserved: next + BLOCK,
            latest_completed: next - 1,
            running: HashMap::new(),
            commands: HashMap::new(),
        },
    );
    secantus_pgplan::snapshots::set_xid_hook(answer);
    Ok(())
}

/// The next xid, reserving another block first when this one is used up.
fn take(c: &mut Cluster) -> u64 {
    let xid = c.next;
    c.next += 1;
    if xid >= c.reserved {
        if let Some(storage) = c.storage.upgrade() {
            match reserve(&storage, xid + BLOCK) {
                Ok(()) => c.reserved = xid + BLOCK,
                // The xid is still unique in this run; only a restart could
                // hand it out again. Said, not dropped.
                Err(e) => eprintln!("secantusd-pg: {e}"),
            }
        }
    }
    xid
}

fn answer(pid: i32, op: XidOp) -> Option<String> {
    let mut all = clusters().lock().unwrap_or_else(|e| e.into_inner());
    let c = all.get_mut(&CURRENT.with(Cell::get))?;
    match op {
        XidOp::Assign => {
            if let Some(x) = c.running.get(&pid) {
                return Some(x.to_string());
            }
            let xid = take(c);
            c.running.insert(pid, xid);
            Some(xid.to_string())
        }
        XidOp::IfAssigned => c.running.get(&pid).map(u64::to_string),
        XidOp::Stable => Some(c.running.get(&pid).copied().unwrap_or(c.next).to_string()),
        XidOp::Snapshot => {
            let xmax = c.latest_completed + 1;
            let mut xip: Vec<u64> = c
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

/// `pid`'s transaction on `storage` ended: its xid, if it had one, is
/// complete. Two servers in one process can share a pid, so the store says
/// whose.
pub fn end(storage: &Arc<Storage>, pid: i32) {
    let mut all = clusters().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = all.get_mut(&key(storage)) {
        if let Some(xid) = c.running.remove(&pid) {
            c.latest_completed = c.latest_completed.max(xid);
        }
        c.commands.remove(&pid);
    }
}

/// Every backend with a running (assigned, not completed) xid on this
/// store's cluster: `(pid, xid)`, sorted by pid — `pg_locks`' granted
/// transactionid rows.
pub fn running() -> Vec<(i32, u64)> {
    let all = clusters().lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<(i32, u64)> = all
        .get(&CURRENT.with(Cell::get))
        .map(|c| c.running.iter().map(|(p, x)| (*p, *x)).collect())
        .unwrap_or_default();
    out.sort_unstable();
    out
}

/// The command id a row write of `pid`'s transaction stamps (`cmin`): its
/// transaction's current one, marked used.
pub fn command_id(storage: &Arc<Storage>, pid: i32) -> u32 {
    let mut all = clusters().lock().unwrap_or_else(|e| e.into_inner());
    let Some(c) = all.get_mut(&key(storage)) else {
        return 0;
    };
    let entry = c.commands.entry(pid).or_insert((0, false));
    entry.1 = true;
    entry.0
}

/// A statement of `pid`'s transaction ended, having consumed `n` command
/// ids (`CommandCounterIncrement`): a row write the one it used.
pub fn advance_command(storage: &Arc<Storage>, pid: i32, n: u32) {
    if n == 0 {
        return;
    }
    let mut all = clusters().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = all.get_mut(&key(storage)) {
        let entry = c.commands.entry(pid).or_insert((0, false));
        entry.0 += n;
        entry.1 = false;
    }
}
