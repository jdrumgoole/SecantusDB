//! A connection's last op, and what mongod does with it as a write ends
//! and waits for its write concern -- with the interrupt checks that
//! counts (RM-360).
//!
//! mongod keeps, for each client, the optime of the latest oplog entry its
//! writes logged (`ReplClientInfo::getLastOp`, set as each write's unit of
//! work commits); a write concern waits for that optime. A write that
//! logged nothing -- an update that matched nothing, a duplicate key --
//! moves it to the system's latest instead
//! (`ReplClientInfo::setLastOpToSystemLastOpTime`), which reads the oplog
//! under the global lock in IS (`getLatestWriteOpTime`). `insert`, `update`
//! and `delete` do so when their last statement logged nothing
//! (`LastOpFixer`, whose destructor fixes unless that statement finished
//! and moved the last op), and the entry point does so again when the
//! command left the last op where it found it
//! (`service_entry_point_shard_role_helpers::waitForWriteConcern`), so a
//! write that logged nothing on a connection whose last op was already the
//! system's reads it twice. Each read's two locks count a check each, as
//! every lock does (`Locker::_lockBegin`).

use std::cell::Cell;

use bson::{Bson, Document};

use crate::CommandContext;

thread_local! {
    /// The statement a write command last started (`LastOpFixer::startingOp`).
    static STATEMENT: Cell<Option<usize>> = const { Cell::new(None) };
    /// Whether the running command's writes have logged anything.
    static LOGGED: Cell<bool> = const { Cell::new(false) };
    /// The latest seq the running command's writes logged, not yet taken
    /// into its connection's last op.
    static PENDING: Cell<Option<i64>> = const { Cell::new(None) };
    /// How many documents the running command inserts, when an `insert`.
    static DOCUMENTS: Cell<usize> = const { Cell::new(0) };
}

/// A command begins: nothing logged, no statement started, and the
/// connection's last op as its last command left it.
pub(crate) fn begin(ctx: &mut CommandContext, doc: &Document) {
    if let Some(auth) = &ctx.conn_auth {
        ctx.last_op_seq = auth.lock().unwrap_or_else(|e| e.into_inner()).last_op_seq;
    }
    STATEMENT.with(|s| s.set(None));
    LOGGED.with(|l| l.set(false));
    PENDING.with(|p| p.set(None));
    let documents = match &ctx.raw_insert_documents {
        Some(raw) => raw.len(),
        None => doc.get_array("documents").map_or(0, Vec::len),
    };
    DOCUMENTS.with(|d| d.set(documents));
    // What an earlier command logged was taken when it ended; nothing
    // logged on this thread since belongs to this one.
    if let Some(storage) = &ctx.storage {
        storage.take_last_logged_seq();
    }
}

/// The command is done with the connection's last op: kept for its next.
pub(crate) fn end(ctx: &CommandContext) {
    if let Some(auth) = &ctx.conn_auth {
        auth.lock().unwrap_or_else(|e| e.into_inner()).last_op_seq = ctx.last_op_seq;
    }
}

/// Take what this thread's writes logged since the last take; whether
/// they logged anything.
fn take(storage: &dyn crate::Storage) -> bool {
    let Some(seq) = storage.take_last_logged_seq() else {
        return false;
    };
    PENDING.with(|p| p.set(Some(p.get().map_or(seq, |s| s.max(seq)))));
    LOGGED.with(|l| l.set(true));
    true
}

/// Take what this thread's writes logged into the connection's last op;
/// whether they logged anything since the last take.
pub(crate) fn fold(ctx: &mut CommandContext) -> bool {
    let logged = ctx.storage.as_ref().is_some_and(|s| take(s.as_ref()));
    if let Some(seq) = PENDING.with(Cell::take) {
        ctx.last_op_seq = ctx.last_op_seq.max(seq);
    }
    logged
}

/// A write command starts its statement `index` (`LastOpFixer::startingOp`).
pub(crate) fn starting_op(storage: &dyn crate::Storage, index: usize) {
    take(storage);
    STATEMENT.with(|s| s.set(Some(index)));
}

/// `setLastOpToSystemLastOpTime`: the connection's last op becomes the
/// oplog's latest, read under the global lock in IS.
pub(crate) fn set_last_op_to_system(ctx: &mut CommandContext) {
    let locks = crate::lock_manager::latest_write_optime_locks(ctx.locks.as_ref());
    if let Some(storage) = &ctx.storage {
        ctx.last_op_seq = ctx.last_op_seq.max(storage.oplog_tail_seq());
    }
    drop(locks);
}

/// Whether the entry point moves the connection's last op to the system's
/// for a command other than `insert`, `update`, `delete` and
/// `findAndModify` (which [`finish_write`] fixes): mongod's
/// `waitForWriteConcern` does so for a command that waits for a write
/// concern, outside a transaction, when the command left the last op where
/// it found it and took the global lock in a write mode
/// (`GlobalLockAcquisitionTracker::getGlobalWriteLocked`) -- a write that
/// logged nothing, as a `create` of a collection there does (measured
/// 8.2.11: one more transition IX and global IS on its line; RM-357).
pub(crate) fn entry_point_fixes(
    name: &str,
    doc: &Document,
    ctx: &CommandContext,
    before: i64,
) -> bool {
    ctx.replica_set_name.is_some()
        && !doc.contains_key("autocommit")
        && ctx.db_name != "local"
        && crate::takes_default_write_concern(name, doc)
        && ctx.last_op_seq == before
        && crate::lock_manager::global_write_locked()
}

/// The connection's last op moved to the oplog's latest, its read's locks
/// left to the caller: `createIndexes`, whose read of it (after its build)
/// is among the global IS rounds its line already counts as mongod's
/// (RM-185).
pub(crate) fn move_to_system(ctx: &mut CommandContext) {
    if let Some(storage) = &ctx.storage {
        ctx.last_op_seq = ctx.last_op_seq.max(storage.oplog_tail_seq());
    }
}

/// The index of each statement `reply` reports failed.
fn failed_statements(reply: &Document) -> impl Iterator<Item = i64> + '_ {
    reply
        .get_array("writeErrors")
        .into_iter()
        .flatten()
        .filter_map(|e| e.as_document()?.get("index").and_then(crate::util::as_i64))
}

/// Whether an insert's last statement failed: mongod's batch stops at an
/// ordered insert's first failure, and an unordered one tries every
/// document, so its last failed when the last document did.
fn insert_last_failed(doc: &Document, reply: &Document) -> bool {
    let failed: Vec<i64> = failed_statements(reply).collect();
    if failed.is_empty() {
        return false;
    }
    let ordered = !matches!(doc.get("ordered"), Some(Bson::Boolean(false)));
    let inserted = reply.get("n").and_then(crate::util::as_i64).unwrap_or(0);
    ordered || failed.iter().max() == Some(&(inserted + failed.len() as i64 - 1))
}

/// A write command ran, outside a transaction, on a member of a replica
/// set: its `LastOpFixer`, then the entry point's, as mongod's run before
/// it waits for its write concern. `before` is the connection's last op
/// when the command began. Returns whether the command's writes logged
/// anything of their own.
pub(crate) fn finish_write(
    name: &str,
    doc: &Document,
    reply: &Document,
    ctx: &mut CommandContext,
    before: i64,
) -> bool {
    let last_logged = fold(ctx);
    let logged = LOGGED.with(Cell::get);
    let statement = STATEMENT.with(Cell::take);
    // `local` is not replicated: no last op to fix, no write concern.
    if ctx.db_name == "local" {
        return logged;
    }
    // mongod inserts a batch of documents in one unit of work, and when it
    // fails inserts them again one at a time, each under locks of its own
    // (`insertBatchAndHandleErrors`); this server takes no such rounds
    // (RM-357), so neither their locks nor their checks are mongod's.
    if name == "insert"
        && DOCUMENTS.with(Cell::get) > 1
        && failed_statements(reply).next().is_some()
    {
        secantus_core::deadline::unmatched();
    }
    let ran = reply.get("ok").and_then(crate::util::as_i64) == Some(1);
    // Whether the last statement finished and moved the last op
    // (`LastOpFixer::finishedOpSuccessfully`); the fixer fixes unless it did.
    let moved = match name {
        "insert" => !insert_last_failed(doc, reply) && logged,
        "update" | "delete" => statement
            .is_some_and(|i| last_logged && !failed_statements(reply).any(|f| f == i as i64)),
        _ => true,
    };
    if ran && matches!(name, "insert" | "update" | "delete") && !moved {
        set_last_op_to_system(ctx);
    }
    if ctx.last_op_seq == before {
        set_last_op_to_system(ctx);
    }
    logged
}

/// How a write concern waits (mongod's `WriteConcernOptions::SyncMode`,
/// with `writeConcernMajorityJournalDefault` filling an unset one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitShape {
    /// `w: 1` and no `j`: nothing to wait for.
    None,
    /// `j: true` with `w: 1`: the oplog's holes, then a journal flush.
    Journal,
    /// `w: "majority"` (`j` unset or true): the oplog's holes, then the
    /// majority commit point.
    Majority,
    /// A shape whose checks are not where mongod's are yet: `fsync`, a
    /// `majority` with `j: false`, a numbered `w` above 1, a tag, `w: 0`.
    Unmatched,
}

pub(crate) fn wait_shape(client_wc: Option<&Document>, default_wc: Option<&Document>) -> WaitShape {
    let client = client_wc.filter(|wc| !wc.is_empty());
    let wc = client.or(default_wc);
    let flag = |key: &str| {
        wc.and_then(|wc| wc.get(key)).map(|v| match v {
            Bson::Boolean(b) => *b,
            other => crate::util::as_i64(other).is_some_and(|n| n != 0),
        })
    };
    if flag("fsync") == Some(true) {
        return WaitShape::Unmatched;
    }
    let majority = match wc.and_then(|wc| wc.get("w")) {
        None => true,
        Some(Bson::String(w)) if w == "majority" => true,
        Some(Bson::String(_)) => return WaitShape::Unmatched,
        Some(w) => match crate::util::as_i64(w) {
            Some(1) => false,
            _ => return WaitShape::Unmatched,
        },
    };
    match (majority, flag("j")) {
        (true, Some(false)) => WaitShape::Unmatched,
        (true, _) => WaitShape::Majority,
        (false, Some(true)) => WaitShape::Journal,
        (false, _) => WaitShape::None,
    }
}

/// A wait mongod makes on a condition, interruptibly
/// (`Interruptible::waitForConditionOrInterruptUntil`): a check before it
/// waits, and one at each wake -- here one, when the condition is not met
/// already. `Err` when the operation was killed or ran out of time.
fn interruptible_wait(waits: bool) -> Result<(), secantus_core::deadline::MaxTimeMsExpired> {
    secantus_core::deadline::check_for_interrupt()?;
    if waits {
        secantus_core::deadline::check_for_interrupt()?;
    }
    Ok(())
}

/// The waits of mongod's `waitForWriteConcern` once it holds the oplog's
/// read locks (`AutoGetOplogFastPath`, the caller's journal-wait locks):
/// that the oplog has no holes below the write
/// (`waitForAllEarlierOplogWritesToBeVisible`, which waits once for a
/// write's own entry -- the visibility thread publishes it after the
/// commit, measured 8.2.11 -- and not for an older one), then for `j` the
/// journal flush it triggers and waits for (`waitForJournalFlush`), or for
/// `majority` the flush it triggers and the commit point
/// (`awaitReplication`: a check of its own, then a wait, which a write
/// the majority already holds -- the system's latest, for one that logged
/// nothing -- does not make). `logged`: the write logged entries of its
/// own. `flush` is the journal flush, called where mongod's happens; a
/// check that finds the operation interrupted ends the checks, as mongod's
/// wait throws there, and the caller still flushes.
pub(crate) fn write_concern_checks(shape: WaitShape, logged: bool, flush: &mut dyn FnMut()) {
    let mut checked = || -> Result<(), secantus_core::deadline::MaxTimeMsExpired> {
        interruptible_wait(logged)?;
        match shape {
            WaitShape::Journal => {
                secantus_core::deadline::check_for_interrupt()?;
                flush();
                secantus_core::deadline::check_for_interrupt()
            }
            WaitShape::Majority => {
                flush();
                secantus_core::deadline::check_for_interrupt()?;
                if logged {
                    interruptible_wait(true)?;
                }
                Ok(())
            }
            WaitShape::None | WaitShape::Unmatched => {
                flush();
                Ok(())
            }
        }
    };
    // An interruption is recorded where it was found (`deadline::tripped`)
    // and answered by `interrupt::finish`.
    if checked().is_err() {
        debug_assert!(secantus_core::deadline::tripped());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn the_wait_follows_the_write_concern() {
        let shape = |wc: Option<Document>| wait_shape(wc.as_ref(), None);
        assert_eq!(shape(None), WaitShape::Majority, "the implicit majority");
        assert_eq!(shape(Some(doc! {})), WaitShape::Majority);
        assert_eq!(shape(Some(doc! {"w": 1})), WaitShape::None);
        assert_eq!(shape(Some(doc! {"w": 1, "j": false})), WaitShape::None);
        assert_eq!(shape(Some(doc! {"w": 1, "j": true})), WaitShape::Journal);
        assert_eq!(shape(Some(doc! {"w": "majority"})), WaitShape::Majority);
        assert_eq!(
            shape(Some(doc! {"w": "majority", "j": true})),
            WaitShape::Majority
        );
        assert_eq!(
            shape(Some(doc! {"w": "majority", "j": false})),
            WaitShape::Unmatched
        );
        assert_eq!(
            shape(Some(doc! {"w": 1, "fsync": true})),
            WaitShape::Unmatched
        );
        assert_eq!(shape(Some(doc! {"w": 2})), WaitShape::Unmatched);
        assert_eq!(shape(Some(doc! {"w": 0})), WaitShape::Unmatched);
        assert_eq!(shape(Some(doc! {"w": "dc"})), WaitShape::Unmatched);
        assert_eq!(
            wait_shape(None, Some(&doc! {"w": 1})),
            WaitShape::None,
            "a stored default"
        );
    }

    #[test]
    fn an_inserts_last_statement_is_where_it_stopped() {
        let failed_at = |i: i32| doc! {"index": i, "code": 11000};
        let ordered = doc! {"insert": "c"};
        let unordered = doc! {"insert": "c", "ordered": false};
        assert!(!insert_last_failed(&ordered, &doc! {"n": 3, "ok": 1.0}));
        assert!(insert_last_failed(
            &ordered,
            &doc! {"n": 1, "writeErrors": [failed_at(1)], "ok": 1.0}
        ));
        assert!(!insert_last_failed(
            &unordered,
            &doc! {"n": 2, "writeErrors": [failed_at(1)], "ok": 1.0}
        ));
        assert!(insert_last_failed(
            &unordered,
            &doc! {"n": 2, "writeErrors": [failed_at(2)], "ok": 1.0}
        ));
    }
}
