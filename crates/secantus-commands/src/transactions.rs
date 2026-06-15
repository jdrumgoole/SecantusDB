//! Multi-document transaction registry — Rust port of
//! `src/secantus/transactions.py`.
//!
//! MongoDB drivers run a transaction as a sequence of ordinary commands that all
//! carry the same envelope: `lsid` (logical session id), `txnNumber` (monotonic
//! per session), and `autocommit: false`. There is no standalone
//! `startTransaction` command — the first statement carries
//! `startTransaction: true`, and the transaction ends with a
//! `commitTransaction` / `abortTransaction` admin command carrying the same
//! `lsid` + `txnNumber`.
//!
//! [`TransactionRegistry`] is the server-side state machine for that protocol.
//! It is storage-agnostic: the WiredTiger work (begin / commit / rollback of the
//! underlying WT transaction) is injected as `commit` / `rollback` callables, so
//! the registry can be unit-tested with fakes (mirroring the Python registry's
//! injectable `commit_func` / `rollback_func`).
//!
//! State machine (spec-pinned by pymongo's `transactions/unified` suite):
//! * statement for an unknown / aborted `(lsid, txnNumber)` → 251
//!   `NoSuchTransaction` + `TransientTransactionError` label
//! * statement for a committed `txnNumber` → 256 `TransactionCommitted`
//! * `startTransaction` with a `txnNumber` lower than the session's newest → 225
//!   `TransactionTooOld`
//! * re-`startTransaction` of the in-progress `txnNumber` → 50911
//! * `startTransaction` with a higher `txnNumber` while an older transaction is
//!   in progress → the older one is implicitly aborted
//! * `commitTransaction` on a committed transaction → `{ok: 1}` (commits are
//!   retried by drivers; idempotency is load-bearing)
//! * `commitTransaction` on an aborted / unknown transaction → 251 + label
//! * `abortTransaction` on an aborted / unknown transaction → 251 with NO label
//!
//! Transactions idle past `lifetime_seconds` (default 60) are reaped
//! opportunistically on every registry access — same no-background-sweeper
//! pattern as cursors. Connection close does NOT abort a transaction: pymongo may
//! legally send a transaction's statements and its (retryable) commit on
//! different pooled connections.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bson::{doc, Document};

/// mongod default `transactionLifetimeLimitSeconds`.
pub const DEFAULT_LIFETIME_SECONDS: f64 = 60.0;

pub const TRANSIENT_LABEL: &str = "TransientTransactionError";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnState {
    InProgress,
    Committed,
    Aborted,
}

/// One transaction's server-side state. `handle` is the opaque storage-side
/// transaction handle (the WT session wrapper), created lazily by the command
/// layer at the first statement so the WT snapshot pins there.
pub struct Transaction {
    pub lsid_bytes: Vec<u8>,
    pub txn_number: i64,
    pub state: TxnState,
    /// The storage `UserTransactionHandle`, boxed opaque. `None` until the first
    /// statement creates it.
    pub handle: Option<Box<dyn Any + Send>>,
    pub last_use: f64,
    #[allow(dead_code)]
    pub started_at: f64,
}

impl std::fmt::Debug for Transaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transaction")
            .field("txn_number", &self.txn_number)
            .field("state", &self.state)
            .field("has_handle", &self.handle.is_some())
            .finish()
    }
}

impl Transaction {
    fn new(lsid_bytes: Vec<u8>, txn_number: i64, now: f64) -> Self {
        Transaction {
            lsid_bytes,
            txn_number,
            state: TxnState::InProgress,
            handle: None,
            last_use: now,
            started_at: now,
        }
    }
}

/// `251 NoSuchTransaction`, optionally carrying the transient label.
pub fn no_such_transaction_reply(txn_number: i64, label: bool) -> Document {
    let mut err = doc! {
        "ok": 0.0,
        "errmsg": format!(
            "Given transaction number {txn_number} does not match any in-progress transactions."
        ),
        "code": 251,
        "codeName": "NoSuchTransaction",
    };
    if label {
        err.insert("errorLabels", vec![TRANSIENT_LABEL]);
    }
    err
}

fn transaction_committed(txn_number: i64) -> Document {
    doc! {
        "ok": 0.0,
        "errmsg": format!("Transaction with {{ txnNumber: {txn_number} }} has been committed."),
        "code": 256,
        "codeName": "TransactionCommitted",
    }
}

fn transaction_too_old(txn_number: i64, newest: i64) -> Document {
    doc! {
        "ok": 0.0,
        "errmsg": format!(
            "Cannot start transaction {txn_number} on session because a newer transaction \
             {newest} has already started."
        ),
        "code": 225,
        "codeName": "TransactionTooOld",
    }
}

fn cannot_restart(txn_number: i64) -> Document {
    doc! {
        "ok": 0.0,
        "errmsg": format!(
            "Cannot restart transaction {txn_number}: a transaction with the same number is \
             already in progress or has finished."
        ),
        "code": 50911,
        "codeName": "Location50911",
    }
}

type TxnFn = Box<dyn Fn(&mut Transaction) + Send + Sync>;
type Clock = Box<dyn Fn() -> f64 + Send + Sync>;

struct Inner {
    txns: HashMap<Vec<u8>, Arc<Mutex<Transaction>>>,
    /// Newest `txnNumber` ever seen per session (transactions and retryable
    /// writes share the per-session sequence).
    last_number: HashMap<Vec<u8>, i64>,
}

/// Thread-safe map of `lsid_bytes` → most-recent [`Transaction`].
///
/// Lock discipline mirrors the Python registry: the registry `inner` mutex
/// guards the maps; each transaction's own mutex guards its `state` / `handle`.
/// The order is always inner → txn (or txn alone), never txn → inner, so
/// statement execution can't deadlock with the reaper / `abort_all`.
pub struct TransactionRegistry {
    inner: Mutex<Inner>,
    commit: TxnFn,
    rollback: TxnFn,
    lifetime: f64,
    clock: Clock,
}

impl TransactionRegistry {
    /// `commit` / `rollback` perform the storage-side WT work for a transaction;
    /// `clock` returns monotonic seconds (injectable for deterministic tests).
    pub fn new(commit: TxnFn, rollback: TxnFn, lifetime_seconds: f64, clock: Clock) -> Self {
        TransactionRegistry {
            inner: Mutex::new(Inner {
                txns: HashMap::new(),
                last_number: HashMap::new(),
            }),
            commit,
            rollback,
            lifetime: lifetime_seconds,
            clock,
        }
    }

    /// Resolve the transaction a statement should run in. `Ok(txn)` to execute
    /// inside `txn`; `Err(reply)` when the envelope is invalid for the session's
    /// current state. The caller must re-validate `txn.state` under the txn lock
    /// before running (the reaper may abort between resolution and execution).
    pub fn for_statement(
        &self,
        lsid_bytes: &[u8],
        txn_number: i64,
        start: bool,
    ) -> Result<Arc<Mutex<Transaction>>, Document> {
        let now = (self.clock)();
        let mut inner = self.inner.lock().unwrap();
        self.prune_locked(&mut inner, now);
        let cur = inner.txns.get(lsid_bytes).cloned();
        let last = inner.last_number.get(lsid_bytes).copied().unwrap_or(0);
        if start {
            if txn_number < last {
                return Err(transaction_too_old(txn_number, last));
            }
            if let Some(c) = &cur {
                if c.lock().unwrap().txn_number == txn_number {
                    if c.lock().unwrap().state == TxnState::Committed {
                        return Err(transaction_committed(txn_number));
                    }
                    return Err(cannot_restart(txn_number));
                }
            }
            if txn_number == last && cur.is_none() {
                // The number was consumed by a retryable write.
                return Err(cannot_restart(txn_number));
            }
            if let Some(c) = &cur {
                self.abort_locked(c);
            }
            let txn = Arc::new(Mutex::new(Transaction::new(
                lsid_bytes.to_vec(),
                txn_number,
                now,
            )));
            inner.txns.insert(lsid_bytes.to_vec(), txn.clone());
            inner.last_number.insert(lsid_bytes.to_vec(), txn_number);
            return Ok(txn);
        }
        // Continuation statement (no startTransaction flag).
        match cur {
            Some(c) if c.lock().unwrap().txn_number == txn_number => {
                let mut t = c.lock().unwrap();
                match t.state {
                    TxnState::Committed => Err(transaction_committed(txn_number)),
                    TxnState::Aborted => Err(no_such_transaction_reply(txn_number, true)),
                    TxnState::InProgress => {
                        t.last_use = now;
                        drop(t);
                        Ok(c)
                    }
                }
            }
            _ => Err(no_such_transaction_reply(txn_number, true)),
        }
    }

    /// Commit `(lsid, txnNumber)`. `Ok(None)` on success (or idempotent re-commit
    /// of a committed txn); `Ok(Some(reply))` for a state error. If the commit
    /// callback panics the txn is left aborted — but we can't catch a panic
    /// cleanly here, so the callback must surface failures via its own state.
    pub fn commit(&self, lsid_bytes: &[u8], txn_number: i64) -> Option<Document> {
        let now = (self.clock)();
        let cur = {
            let mut inner = self.inner.lock().unwrap();
            self.prune_locked(&mut inner, now);
            inner.txns.get(lsid_bytes).cloned()
        };
        let Some(c) = cur else {
            return Some(no_such_transaction_reply(txn_number, true));
        };
        let mut t = c.lock().unwrap();
        if t.txn_number != txn_number {
            return Some(no_such_transaction_reply(txn_number, true));
        }
        match t.state {
            TxnState::Committed => None,
            TxnState::Aborted => Some(no_such_transaction_reply(txn_number, true)),
            TxnState::InProgress => {
                (self.commit)(&mut t);
                t.state = TxnState::Committed;
                t.last_use = now;
                None
            }
        }
    }

    /// Abort `(lsid, txnNumber)`. `Ok(None)` on success; `Ok(Some(reply))` for a
    /// state error. No transient label — drivers fire-and-forget aborts.
    pub fn abort(&self, lsid_bytes: &[u8], txn_number: i64) -> Option<Document> {
        let now = (self.clock)();
        let cur = {
            let mut inner = self.inner.lock().unwrap();
            self.prune_locked(&mut inner, now);
            inner.txns.get(lsid_bytes).cloned()
        };
        let Some(c) = cur else {
            return Some(no_such_transaction_reply(txn_number, false));
        };
        let mut t = c.lock().unwrap();
        if t.txn_number != txn_number {
            return Some(no_such_transaction_reply(txn_number, false));
        }
        match t.state {
            TxnState::Committed => Some(transaction_committed(txn_number)),
            TxnState::Aborted => Some(no_such_transaction_reply(txn_number, false)),
            TxnState::InProgress => {
                (self.rollback)(&mut t);
                t.state = TxnState::Aborted;
                t.last_use = now;
                None
            }
        }
    }

    /// Server-side abort after a failed statement (mongod parity: any failed
    /// statement aborts the transaction). No-op once terminal.
    pub fn abort_in_progress(&self, txn: &Arc<Mutex<Transaction>>) {
        self.abort_locked(txn);
    }

    /// A retryable write (`txnNumber` without `autocommit`) consumes the
    /// session's txnNumber sequence and implicitly aborts an older in-progress
    /// transaction, as in mongod.
    pub fn on_retryable_write(&self, lsid_bytes: &[u8], txn_number: i64) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(c) = inner.txns.get(lsid_bytes).cloned() {
            if txn_number > c.lock().unwrap().txn_number {
                self.abort_locked(&c);
                inner.txns.remove(lsid_bytes);
            }
        }
        let last = inner.last_number.get(lsid_bytes).copied().unwrap_or(0);
        if txn_number > last {
            inner.last_number.insert(lsid_bytes.to_vec(), txn_number);
        }
    }

    /// `endSessions` / `killSessions`: abort the session's in-progress txn.
    pub fn abort_for_session(&self, lsid_bytes: &[u8]) {
        let cur = self.inner.lock().unwrap().txns.get(lsid_bytes).cloned();
        if let Some(c) = cur {
            self.abort_locked(&c);
        }
    }

    /// `killAllSessions` / shutdown: abort everything.
    pub fn abort_all(&self) {
        let txns: Vec<_> = self.inner.lock().unwrap().txns.values().cloned().collect();
        for c in txns {
            self.abort_locked(&c);
        }
    }

    /// Count of in-progress transactions (test/introspection helper).
    pub fn in_progress_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .txns
            .values()
            .filter(|c| c.lock().unwrap().state == TxnState::InProgress)
            .count()
    }

    fn abort_locked(&self, txn: &Arc<Mutex<Transaction>>) {
        let mut t = txn.lock().unwrap();
        if t.state == TxnState::InProgress {
            (self.rollback)(&mut t);
            t.state = TxnState::Aborted;
        }
    }

    fn prune_locked(&self, inner: &mut Inner, now: f64) {
        let cutoff = now - self.lifetime;
        for c in inner.txns.values() {
            let mut t = c.lock().unwrap();
            if t.state == TxnState::InProgress && t.last_use < cutoff {
                (self.rollback)(&mut t);
                t.state = TxnState::Aborted;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A registry with no-op commit/rollback and a manually-advanced clock.
    fn reg() -> (Arc<TransactionRegistry>, Arc<AtomicU64>, Arc<AtomicU64>) {
        let commits = Arc::new(AtomicU64::new(0));
        let rollbacks = Arc::new(AtomicU64::new(0));
        let c2 = commits.clone();
        let r2 = rollbacks.clone();
        let r = TransactionRegistry::new(
            Box::new(move |_t| {
                c2.fetch_add(1, Ordering::SeqCst);
            }),
            Box::new(move |_t| {
                r2.fetch_add(1, Ordering::SeqCst);
            }),
            60.0,
            Box::new(|| 0.0),
        );
        (Arc::new(r), commits, rollbacks)
    }

    const LS: &[u8] = b"session-1";

    #[test]
    fn start_then_continue_then_commit() {
        let (r, commits, _) = reg();
        assert!(r.for_statement(LS, 1, true).is_ok());
        assert!(r.for_statement(LS, 1, false).is_ok());
        assert!(r.commit(LS, 1).is_none());
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        // Re-commit is idempotent (drivers retry commits).
        assert!(r.commit(LS, 1).is_none());
        assert_eq!(commits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn continuation_for_unknown_txn_is_no_such_transaction() {
        let (r, _, _) = reg();
        let err = r.for_statement(LS, 7, false).unwrap_err();
        assert_eq!(err.get_i32("code").unwrap(), 251);
        assert!(err
            .get_array("errorLabels")
            .unwrap()
            .iter()
            .any(|l| l.as_str() == Some(TRANSIENT_LABEL)));
    }

    #[test]
    fn statement_after_commit_is_transaction_committed() {
        let (r, _, _) = reg();
        r.for_statement(LS, 1, true).unwrap();
        r.commit(LS, 1).unwrap_none_ok();
        let err = r.for_statement(LS, 1, false).unwrap_err();
        assert_eq!(err.get_i32("code").unwrap(), 256);
    }

    #[test]
    fn restart_in_progress_is_50911() {
        let (r, _, _) = reg();
        r.for_statement(LS, 1, true).unwrap();
        let err = r.for_statement(LS, 1, true).unwrap_err();
        assert_eq!(err.get_i32("code").unwrap(), 50911);
    }

    #[test]
    fn older_start_is_transaction_too_old() {
        let (r, _, _) = reg();
        r.for_statement(LS, 5, true).unwrap();
        let err = r.for_statement(LS, 3, true).unwrap_err();
        assert_eq!(err.get_i32("code").unwrap(), 225);
    }

    #[test]
    fn higher_start_aborts_older_in_progress() {
        let (r, _, rollbacks) = reg();
        r.for_statement(LS, 1, true).unwrap();
        assert_eq!(r.in_progress_count(), 1);
        r.for_statement(LS, 2, true).unwrap();
        // The older txn was implicitly rolled back.
        assert_eq!(rollbacks.load(Ordering::SeqCst), 1);
        assert_eq!(r.in_progress_count(), 1);
    }

    #[test]
    fn abort_unknown_has_no_label() {
        let (r, _, _) = reg();
        let err = r.abort(LS, 9).unwrap();
        assert_eq!(err.get_i32("code").unwrap(), 251);
        assert!(err.get("errorLabels").is_none());
    }

    #[test]
    fn abort_in_progress_rolls_back() {
        let (r, _, rollbacks) = reg();
        let txn = r.for_statement(LS, 1, true).unwrap();
        r.abort_in_progress(&txn);
        assert_eq!(rollbacks.load(Ordering::SeqCst), 1);
        // A continuation now sees NoSuchTransaction.
        assert_eq!(
            r.for_statement(LS, 1, false)
                .unwrap_err()
                .get_i32("code")
                .unwrap(),
            251
        );
    }

    #[test]
    fn retryable_write_consumes_sequence() {
        let (r, _, _) = reg();
        r.on_retryable_write(LS, 4);
        // A later startTransaction with a lower number is too old.
        assert_eq!(
            r.for_statement(LS, 3, true)
                .unwrap_err()
                .get_i32("code")
                .unwrap(),
            225
        );
        // Reusing the consumed number can't start a transaction.
        assert_eq!(
            r.for_statement(LS, 4, true)
                .unwrap_err()
                .get_i32("code")
                .unwrap(),
            50911
        );
    }

    /// Tiny helper so `commit().unwrap_none()` reads cleanly in tests.
    trait OptionExt {
        fn unwrap_none_ok(self);
    }
    impl OptionExt for Option<Document> {
        fn unwrap_none_ok(self) {
            assert!(self.is_none(), "expected ok (None), got {self:?}");
        }
    }
}
