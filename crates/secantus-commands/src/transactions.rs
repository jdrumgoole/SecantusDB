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

use bson::{doc, Bson, Document};

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
    /// Retryable-write records: `(lsid, txnNumber)` -> [`RetryableRecord`]. mongod keeps the equivalent in `config.transactions` so a
    /// driver's automatic retry gets the ORIGINAL reply instead of re-applying
    /// the write; without it a retried `{$inc: {n: 1}}` increments twice while
    /// both replies claim `nModified: 1`. Mirrors the Python registry's
    /// `_retryable`.
    retryable: HashMap<RetryableKey, RetryableRecord>,
}

/// `(lsid, txnNumber)` — the pair mongod keys a retryable write on.
type RetryableKey = (Vec<u8>, i64);

/// A completed retryable write: its reply, when it was stored, and a digest of
/// the command that produced it (so a replay only fires for the same write).
type RetryableRecord = (Document, f64, [u8; 20]);

/// How long a retryable-write record is kept, matching mongod's 30-minute
/// sweep. A driver retrying later than this re-executes — as it would against
/// mongod.
const RETRYABLE_RECORD_LIFETIME_SECONDS: f64 = 30.0 * 60.0;

/// Backstop on record count so a client minting unbounded sessions cannot grow
/// the map without limit. Oldest-first eviction.
const RETRYABLE_RECORD_MAX: usize = 10_000;

/// Whether `reply` represents a write that fully took effect, and is therefore
/// replayable. A failed or partially-failed write must re-execute on retry:
/// caching an error would make a transient failure permanent, and caching a
/// partial batch would report missing documents as written. A
/// `writeConcernError` is deliberately NOT disqualifying — the write applied,
/// only its replication did not confirm, so a retry must not apply it twice.
fn is_recordable_reply(reply: &Document) -> bool {
    let ok = match reply.get("ok") {
        Some(Bson::Double(d)) => *d,
        Some(Bson::Int32(i)) => *i as f64,
        Some(Bson::Int64(i)) => *i as f64,
        _ => 0.0,
    };
    if ok != 1.0 {
        return false;
    }
    !matches!(reply.get("writeErrors"), Some(Bson::Array(a)) if !a.is_empty())
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
                retryable: HashMap::new(),
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
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut inner, now);
        let cur = inner.txns.get(lsid_bytes).cloned();
        let last = inner.last_number.get(lsid_bytes).copied().unwrap_or(0);
        if start {
            if txn_number < last {
                return Err(transaction_too_old(txn_number, last));
            }
            if let Some(c) = &cur {
                if c.lock().unwrap_or_else(|e| e.into_inner()).txn_number == txn_number {
                    if c.lock().unwrap_or_else(|e| e.into_inner()).state == TxnState::Committed {
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
            Some(c) if c.lock().unwrap_or_else(|e| e.into_inner()).txn_number == txn_number => {
                let mut t = c.lock().unwrap_or_else(|e| e.into_inner());
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
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            self.prune_locked(&mut inner, now);
            inner.txns.get(lsid_bytes).cloned()
        };
        let Some(c) = cur else {
            return Some(no_such_transaction_reply(txn_number, true));
        };
        let mut t = c.lock().unwrap_or_else(|e| e.into_inner());
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
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            self.prune_locked(&mut inner, now);
            inner.txns.get(lsid_bytes).cloned()
        };
        let Some(c) = cur else {
            return Some(no_such_transaction_reply(txn_number, false));
        };
        let mut t = c.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = inner.txns.get(lsid_bytes).cloned() {
            if txn_number > c.lock().unwrap_or_else(|e| e.into_inner()).txn_number {
                self.abort_locked(&c);
                inner.txns.remove(lsid_bytes);
            }
        }
        let last = inner.last_number.get(lsid_bytes).copied().unwrap_or(0);
        if txn_number > last {
            inner.last_number.insert(lsid_bytes.to_vec(), txn_number);
        }
    }

    /// The stored reply for an already-executed retryable write, if any.
    ///
    /// A driver retries with the SAME `lsid` + `txnNumber` after a network
    /// blip, a `writeConcernError`, or a stepdown. mongod recognises the repeat
    /// and replays its stored reply rather than executing the write a second
    /// time; `None` means "not seen before, run it".
    ///
    /// `identity` must match the recorded command too. A retry re-sends a
    /// byte-identical command, so a mismatch means the key was reused for a
    /// DIFFERENT write — replaying one command's reply for another would be
    /// worse than the double-apply this exists to prevent, so we execute.
    pub fn retryable_reply(
        &self,
        lsid_bytes: &[u8],
        txn_number: i64,
        identity: &[u8; 20],
    ) -> Option<Document> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.clock)();
        Self::prune_retryable(&mut inner, now);
        match inner.retryable.get(&(lsid_bytes.to_vec(), txn_number)) {
            Some((reply, _at, ident)) if ident == identity => Some(reply.clone()),
            _ => None,
        }
    }

    /// Store `reply` as the outcome of this retryable write. Only successful
    /// writes are recorded (see [`is_recordable_reply`]).
    pub fn record_retryable(
        &self,
        lsid_bytes: &[u8],
        txn_number: i64,
        identity: [u8; 20],
        reply: &Document,
    ) {
        if !is_recordable_reply(reply) {
            return;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.clock)();
        inner.retryable.insert(
            (lsid_bytes.to_vec(), txn_number),
            (reply.clone(), now, identity),
        );
        Self::prune_retryable(&mut inner, now);
    }

    /// Drop records past their lifetime and cap total size. Called on every
    /// lookup / record, so an idle server sheds them without a background
    /// sweeper — the opportunistic pattern the oplog and TTL pruning use.
    fn prune_retryable(inner: &mut Inner, now: f64) {
        let cutoff = now - RETRYABLE_RECORD_LIFETIME_SECONDS;
        inner.retryable.retain(|_k, (_r, at, _i)| *at >= cutoff);
        if inner.retryable.len() > RETRYABLE_RECORD_MAX {
            let excess = inner.retryable.len() - RETRYABLE_RECORD_MAX;
            let mut by_age: Vec<(RetryableKey, f64)> = inner
                .retryable
                .iter()
                .map(|(k, (_r, at, _i))| (k.clone(), *at))
                .collect();
            by_age.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            for (k, _at) in by_age.into_iter().take(excess) {
                inner.retryable.remove(&k);
            }
        }
    }

    /// `endSessions` / `killSessions`: abort the session's in-progress txn.
    pub fn abort_for_session(&self, lsid_bytes: &[u8]) {
        let cur = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .txns
            .get(lsid_bytes)
            .cloned();
        if let Some(c) = cur {
            self.abort_locked(&c);
        }
    }

    /// `killAllSessions` / shutdown: abort everything.
    pub fn abort_all(&self) {
        let txns: Vec<_> = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .txns
            .values()
            .cloned()
            .collect();
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
            .filter(|c| c.lock().unwrap_or_else(|e| e.into_inner()).state == TxnState::InProgress)
            .count()
    }

    fn abort_locked(&self, txn: &Arc<Mutex<Transaction>>) {
        let mut t = txn.lock().unwrap_or_else(|e| e.into_inner());
        if t.state == TxnState::InProgress {
            (self.rollback)(&mut t);
            t.state = TxnState::Aborted;
        }
    }

    fn prune_locked(&self, inner: &mut Inner, now: f64) {
        let cutoff = now - self.lifetime;
        for c in inner.txns.values() {
            let mut t = c.lock().unwrap_or_else(|e| e.into_inner());
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

    /// Retryable-write records: keyed on (lsid, txnNumber) AND the command
    /// identity. Mirrors the Python server's
    /// `test_registry_isolates_sessions_and_verifies_identity`.
    #[test]
    fn retryable_records_are_keyed_by_session_number_and_identity() {
        let (r, _c, _rb) = reg();
        let ident = [1u8; 20];
        let other = [2u8; 20];
        let reply = doc! {"ok": 1.0, "n": 1};

        r.record_retryable(b"session-a", 1, ident, &reply);

        // Same session, same number, same command -> replay.
        assert_eq!(r.retryable_reply(b"session-a", 1, &ident), Some(reply));
        // Different session -> not a retry.
        assert!(r.retryable_reply(b"session-b", 1, &ident).is_none());
        // Different txnNumber -> a new write.
        assert!(r.retryable_reply(b"session-a", 2, &ident).is_none());
        // Same key, DIFFERENT command: replaying here would serve one write's
        // answer for another, which is worse than the double-apply this
        // prevents.
        assert!(r.retryable_reply(b"session-a", 1, &other).is_none());
    }

    /// Only writes that took effect are replayable. Caching a failure would
    /// turn a transient error into a permanent one.
    #[test]
    fn retryable_records_skip_failed_writes() {
        let (r, _c, _rb) = reg();
        let ident = [3u8; 20];

        r.record_retryable(b"s", 1, ident, &doc! {"ok": 0.0, "errmsg": "boom"});
        assert!(r.retryable_reply(b"s", 1, &ident).is_none());

        r.record_retryable(
            b"s",
            2,
            ident,
            &doc! {"ok": 1.0, "writeErrors": [{"index": 0}]},
        );
        assert!(r.retryable_reply(b"s", 2, &ident).is_none());

        // A writeConcernError means the write DID apply — only replication of
        // it did not confirm — so mongod records it and a retry must not apply
        // it twice.
        r.record_retryable(
            b"s",
            3,
            ident,
            &doc! {"ok": 1.0, "n": 1, "writeConcernError": {"code": 64}},
        );
        assert!(r.retryable_reply(b"s", 3, &ident).is_some());
    }

    /// Records age out, so a much later retry re-executes — as against mongod.
    #[test]
    fn retryable_records_expire() {
        use std::sync::atomic::AtomicU64;
        let now = Arc::new(AtomicU64::new(1000));
        let n2 = now.clone();
        let r = TransactionRegistry::new(
            Box::new(|_t| {}),
            Box::new(|_t| {}),
            60.0,
            Box::new(move || n2.load(Ordering::SeqCst) as f64),
        );
        let ident = [4u8; 20];
        r.record_retryable(b"s", 1, ident, &doc! {"ok": 1.0, "n": 1});
        assert!(r.retryable_reply(b"s", 1, &ident).is_some());

        now.fetch_add(31 * 60, Ordering::SeqCst); // past the 30-minute lifetime
        assert!(r.retryable_reply(b"s", 1, &ident).is_none());
    }

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
