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
//! State machine, measured against mongod 8.2.11 (`tools/probes/transactions.py`).
//! A session has ONE active transaction number, shared by transactions and
//! retryable writes, starting at -1:
//! * a number below the active one -> 225 `TransactionTooOld`, on a statement,
//!   a `startTransaction`, a commit, an abort or a retryable write alike
//! * `startTransaction` at the active number -> 117
//!   `ConflictingOperationInProgress`, whatever that number was used for
//! * `startTransaction` above it -> the older transaction is aborted
//! * a statement, commit or abort for a number that is not an open
//!   transaction -> 251 `NoSuchTransaction` with `TransientTransactionError`;
//!   the message says "has been aborted" when that is what happened
//! * a statement or abort for a committed transaction -> 256
//!   `TransactionCommitted`; a commit of one answers `{ok: 1}` again
//! * a retryable write at the number of a transaction -> 217
//!   `IncompleteTransactionHistory`
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

/// The label mongod puts on a `commitTransaction` / `abortTransaction` failure
/// whose code is about reaching the node rather than about the transaction. It
/// tells a driver to retry the COMMIT, where `TRANSIENT_LABEL` tells it to
/// replay the whole transaction — see `failpoints::RETRYABLE_WRITE_CODES`.
pub const RETRYABLE_WRITE_LABEL: &str = "RetryableWriteError";

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
    pub started_at: f64,
    /// The collections this transaction created, as `db.coll`, and whether
    /// it has since put a document in each.
    pub created: HashMap<String, bool>,
    /// The lock manager owner this transaction takes its statement locks
    /// under, so the collection intent locks it accumulates hold to its
    /// end (what a concurrent `drop` / `createIndexes` / `renameCollection`
    /// / `collMod` waits on, RM-084). `None` where no lock manager is wired.
    pub lock_owner: Option<crate::lock_manager::Owner>,
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
            created: HashMap::new(),
            lock_owner: None,
        }
    }
}

/// How mongod prints a session in a transaction error: the id, then the
/// base64 SHA-256 of the user, then two fields a replica set leaves empty.
/// The digest here is the one of no user.
pub fn session_text(lsid: &[u8]) -> String {
    format!(
        "{} - 47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU= -  - ",
        uuid_text(lsid)
    )
}

fn uuid_text(bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if hex.len() != 32 {
        return hex;
    }
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// An error reply with `errorLabels` FIRST, which is where mongod puts them.
fn labelled(code: i32, code_name: &str, errmsg: String, label: Option<&str>) -> Document {
    let mut err = Document::new();
    if let Some(label) = label {
        err.insert("errorLabels", vec![label]);
    }
    err.insert("ok", 0.0);
    err.insert("errmsg", errmsg);
    err.insert("code", code);
    err.insert("codeName", code_name);
    err
}

/// Put `label` on `reply`, keeping `errorLabels` the first key.
pub fn add_label_first(reply: &mut Document, label: &str) {
    let mut labels: Vec<Bson> = match reply.remove("errorLabels") {
        Some(Bson::Array(a)) => a,
        _ => Vec::new(),
    };
    if !labels.iter().any(|l| l.as_str() == Some(label)) {
        labels.push(Bson::String(label.to_string()));
    }
    let rest = std::mem::take(reply);
    reply.insert("errorLabels", labels);
    for (k, v) in rest {
        reply.insert(k, v);
    }
}

/// `251 NoSuchTransaction` for a number that is not the session's open
/// transaction. `active` is the session's active number, -1 when it has none.
pub fn no_such_transaction_reply(lsid: &[u8], txn_number: i64, active: i64) -> Document {
    labelled(
        251,
        "NoSuchTransaction",
        format!(
            "Given transaction number {txn_number} on session {} using txnRetryCounter 0 does \
             not match any in-progress transactions. The active transaction number is {active}",
            session_text(lsid)
        ),
        Some(TRANSIENT_LABEL),
    )
}

fn transaction_aborted(txn_number: i64) -> Document {
    labelled(
        251,
        "NoSuchTransaction",
        format!("Transaction with {{ txnNumber: {txn_number} }} has been aborted."),
        Some(TRANSIENT_LABEL),
    )
}

fn transaction_committed(txn_number: i64) -> Document {
    labelled(
        256,
        "TransactionCommitted",
        format!("Transaction with {{ txnNumber: {txn_number} }} has been committed."),
        None,
    )
}

fn transaction_too_old(lsid: &[u8], txn_number: i64, active: i64, retryable: bool) -> Document {
    let newer = if retryable {
        format!(
            "retryable write with txnNumberAndRetryCounter {{ txnNumber: {active}, \
             txnRetryCounter: -1 }}"
        )
    } else {
        format!(
            "transaction with txnNumberAndRetryCounter {{ txnNumber: {active}, \
             txnRetryCounter: 0 }}"
        )
    };
    labelled(
        225,
        "TransactionTooOld",
        format!(
            "Cannot start transaction with {{ txnNumber: {txn_number} }} on session {} because \
             a newer {newer} has already started on this session.",
            session_text(lsid)
        ),
        None,
    )
}

fn retryable_too_old(lsid: &[u8], txn_number: i64, active: i64, retryable: bool) -> Document {
    let newer = if retryable {
        "retryable write"
    } else {
        "transaction"
    };
    labelled(
        225,
        "TransactionTooOld",
        format!(
            "Retryable write with txnNumber {txn_number} is prohibited on session {} because a \
             newer {newer} with txnNumber {active} has already started on this session.",
            session_text(lsid)
        ),
        None,
    )
}

fn cannot_restart() -> Document {
    labelled(
        117,
        "ConflictingOperationInProgress",
        "Only servers in a sharded cluster can start a new transaction at the active \
         transaction number"
            .to_string(),
        None,
    )
}

fn converted_to_transaction() -> Document {
    labelled(
        217,
        "IncompleteTransactionHistory",
        "Cannot retry a retryable write that has been converted into a transaction".to_string(),
        None,
    )
}

/// What a statement of a retryable write did, kept so a retry of it is
/// answered and not run again. mongod keeps the same in the oplog, and only
/// for a statement that WROTE: one that matched nothing, or changed nothing,
/// runs again on a retry.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtRecord {
    Insert {
        id: Option<Bson>,
    },
    /// An update that modified a document, or upserted one (`upserted` is its
    /// `_id`).
    Update {
        upserted: Option<Bson>,
    },
    Delete,
    /// A `findAndModify` and the reply it gave, whether it removed, and
    /// whether it returned the document after the update (`new`).
    FindAndModify {
        reply: Document,
        remove: bool,
        new: bool,
    },
}

thread_local! {
    /// The statements the write command running on this thread has applied,
    /// while a retryable write is collecting them.
    static STATEMENTS: std::cell::RefCell<Option<Vec<(usize, StmtRecord)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f`, a write command's handler, and return with it the statements it
/// applied: `(index in the command, what it did)`.
pub fn collect_statements<R>(f: impl FnOnce() -> R) -> (R, Vec<(usize, StmtRecord)>) {
    let outer = STATEMENTS.with(|s| s.borrow_mut().replace(Vec::new()));
    let result = f();
    let noted = STATEMENTS.with(|s| std::mem::replace(&mut *s.borrow_mut(), outer));
    (result, noted.unwrap_or_default())
}

/// Called by a write handler when the statement at `index` has written. A
/// no-op unless a retryable write is collecting.
pub fn note_statement(index: usize, record: StmtRecord) {
    STATEMENTS.with(|s| {
        if let Some(noted) = s.borrow_mut().as_mut() {
            noted.push((index, record));
        }
    });
}

/// The statements of a retryable write that have been applied.
///
/// A large insert batch is kept as runs of statement ids, not one record a
/// document: every session holds its last write until its next one, and a
/// record per document of a 100,000-document batch is megabytes a session.
#[derive(Debug, Clone, Default)]
pub struct Applied {
    records: HashMap<i32, StmtRecord>,
    /// `(first, count)` runs of inserted statement ids.
    inserted: Vec<(i32, u32)>,
}

/// Above this many inserts in one command, they are kept as runs.
const INSERT_RUN_THRESHOLD: usize = 16;

impl Applied {
    pub fn contains(&self, id: i32) -> bool {
        self.get(id).is_some()
    }

    /// What statement `id` did, if it has been applied.
    pub fn get(&self, id: i32) -> Option<StmtRecord> {
        if let Some(record) = self.records.get(&id) {
            return Some(record.clone());
        }
        self.inserted
            .iter()
            .any(|(first, count)| {
                let offset = i64::from(id) - i64::from(*first);
                offset >= 0 && offset < i64::from(*count)
            })
            .then_some(StmtRecord::Insert { id: None })
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty() && self.inserted.is_empty()
    }

    fn add(&mut self, records: Vec<(i32, StmtRecord)>) {
        let inserts = records
            .iter()
            .all(|(_, r)| matches!(r, StmtRecord::Insert { .. }));
        if !inserts || records.len() <= INSERT_RUN_THRESHOLD {
            self.records.extend(records);
            return;
        }
        let mut ids: Vec<i32> = records.into_iter().map(|(id, _)| id).collect();
        ids.sort_unstable();
        for id in ids {
            match self.inserted.last_mut() {
                Some((first, count)) if i64::from(*first) + i64::from(*count) == i64::from(id) => {
                    *count += 1
                }
                _ => self.inserted.push((id, 1)),
            }
        }
    }
}

type TxnFn = Box<dyn Fn(&mut Transaction) + Send + Sync>;
/// The storage commit of a transaction. It can FAIL, and a failed commit must
/// be told to the client: it used to be dropped, and the reply said `ok`.
type CommitFn = Box<dyn Fn(&mut Transaction) -> Result<(), crate::StorageError> + Send + Sync>;
type Clock = Box<dyn Fn() -> f64 + Send + Sync>;

enum Kind {
    Txn(Arc<Mutex<Transaction>>),
    Retryable(Applied),
}

/// One session: its active transaction number and what that number is.
struct Session {
    active: i64,
    kind: Kind,
    last_use: f64,
}

struct Inner {
    sessions: HashMap<Vec<u8>, Session>,
    last_sweep: f64,
}

/// How long an idle session's state is kept: mongod's
/// `localLogicalSessionTimeoutMinutes`.
const SESSION_LIFETIME_SECONDS: f64 = 30.0 * 60.0;

/// Thread-safe map of session -> its active transaction or retryable write.
///
/// Lock discipline: the registry `inner` mutex guards the map; each
/// transaction's own mutex guards its `state` / `handle`. The order is always
/// inner -> txn (or txn alone), never txn -> inner, so statement execution
/// cannot deadlock with the reaper or `abort_all`.
pub struct TransactionRegistry {
    inner: Mutex<Inner>,
    commit: CommitFn,
    rollback: TxnFn,
    /// The lock manager whose owner each transaction takes its statement
    /// locks under; released when the transaction ends. Set once by the
    /// server after construction (the registry is built before the shared
    /// state it lives in is fully populated); unit-test registries run
    /// without one, and locking is best-effort there.
    locks: std::sync::OnceLock<Arc<crate::lock_manager::LockManager>>,
    /// Seconds, as `f64` bits: `transactionLifetimeLimitSeconds` changes it
    /// while the server runs.
    lifetime: std::sync::atomic::AtomicU64,
    clock: Clock,
    /// Started, committed, aborted, and the microseconds the ended ones ran:
    /// `serverStatus.transactions` and `opLatencies.transactions`.
    counts: [std::sync::atomic::AtomicI64; 4],
}

/// `serverStatus.transactions`' counts, measured as mongod 8.2.11 keeps them:
/// a transaction is started by its first statement and ends when it commits
/// or aborts (by the client, by a failed statement, by the reaper, or by a
/// newer transaction on its session).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TxnCounts {
    /// Open and running a statement right now.
    pub current_active: i64,
    /// Open and idle.
    pub current_inactive: i64,
    pub total_aborted: i64,
    pub total_committed: i64,
    pub total_started: i64,
    /// How long the ended transactions ran, from their first statement.
    pub latency_micros: i64,
}

impl TransactionRegistry {
    /// `commit` / `rollback` perform the storage-side WT work for a transaction;
    /// `clock` returns monotonic seconds (injectable for deterministic tests).
    pub fn new(commit: CommitFn, rollback: TxnFn, lifetime_seconds: f64, clock: Clock) -> Self {
        TransactionRegistry {
            inner: Mutex::new(Inner {
                sessions: HashMap::new(),
                last_sweep: 0.0,
            }),
            commit,
            rollback,
            locks: std::sync::OnceLock::new(),
            lifetime: std::sync::atomic::AtomicU64::new(lifetime_seconds.to_bits()),
            clock,
            counts: Default::default(),
        }
    }

    /// Wire the lock manager in (once). The server's `bind` calls this on
    /// the registry it built; without it transactions run unlocked, which
    /// is what unit tests want.
    pub fn set_locks(&self, locks: Arc<crate::lock_manager::LockManager>) -> bool {
        self.locks.set(locks).is_ok()
    }

    /// Let go of the collection locks a transaction held to its end.
    /// Idempotent: every terminal path calls it, and `release` on an owner
    /// with no holds is a no-op.
    fn release_locks(&self, t: &Transaction) {
        if let (Some(lm), Some(owner)) = (self.locks.get(), t.lock_owner) {
            lm.release(owner);
        }
    }

    /// A transaction left `InProgress`: count how it ended and how long it ran.
    fn ended(&self, t: &Transaction, committed: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        self.release_locks(t);
        self.counts[if committed { 1 } else { 2 }].fetch_add(1, Relaxed);
        let micros = (((self.clock)() - t.started_at) * 1e6).max(0.0) as i64;
        self.counts[3].fetch_add(micros, Relaxed);
    }

    /// The counts `serverStatus.transactions` reports.
    pub fn counts(&self) -> TxnCounts {
        use std::sync::atomic::Ordering::Relaxed;
        let [started, committed, aborted, micros] =
            std::array::from_fn(|i| self.counts[i].load(Relaxed));
        // A transaction running a statement holds its own lock.
        let active = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sessions
            .values()
            .filter(|s| match &s.kind {
                Kind::Txn(c) => c.try_lock().is_err(),
                Kind::Retryable(_) => false,
            })
            .count() as i64;
        let open = (started - committed - aborted).max(0);
        let active = active.min(open);
        TxnCounts {
            current_active: active,
            current_inactive: open - active,
            total_aborted: aborted,
            total_committed: committed,
            total_started: started,
            latency_micros: micros,
        }
    }

    /// How long a transaction may sit idle before it is aborted, in seconds.
    pub fn lifetime_seconds(&self) -> f64 {
        f64::from_bits(self.lifetime.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// `setParameter transactionLifetimeLimitSeconds`.
    pub fn set_lifetime_seconds(&self, seconds: f64) {
        self.lifetime
            .store(seconds.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }

    fn locked(&self) -> (std::sync::MutexGuard<'_, Inner>, f64) {
        let now = (self.clock)();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut inner, now);
        (inner, now)
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
        let (mut inner, now) = self.locked();
        let (active, retryable) = match inner.sessions.get(lsid_bytes) {
            Some(s) => (s.active, matches!(s.kind, Kind::Retryable(_))),
            None => (-1, false),
        };
        if txn_number < active {
            return Err(transaction_too_old(
                lsid_bytes, txn_number, active, retryable,
            ));
        }
        if start {
            if txn_number == active {
                return Err(cannot_restart());
            }
            // The transaction this one replaces is aborted once the registry
            // is released: it may be running a statement, and waiting for it
            // here would stall every session's command, and the accept loop.
            let replaced = match inner.sessions.get(lsid_bytes) {
                Some(Session {
                    kind: Kind::Txn(old),
                    ..
                }) => Some(old.clone()),
                _ => None,
            };
            let txn = Arc::new(Mutex::new(Transaction::new(
                lsid_bytes.to_vec(),
                txn_number,
                now,
            )));
            if let Some(lm) = self.locks.get() {
                txn.lock().unwrap_or_else(|e| e.into_inner()).lock_owner =
                    Some(lm.transaction_owner());
            }
            self.counts[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            inner.sessions.insert(
                lsid_bytes.to_vec(),
                Session {
                    active: txn_number,
                    kind: Kind::Txn(txn.clone()),
                    last_use: now,
                },
            );
            drop(inner);
            if let Some(old) = replaced {
                self.abort_locked(&old);
            }
            return Ok(txn);
        }
        // Continuation statement (no startTransaction flag). The transaction
        // is locked only after the registry is released: a statement of its
        // own may hold it while it waits for a lock.
        let c = match inner.sessions.get_mut(lsid_bytes) {
            Some(session) if session.active == txn_number => {
                session.last_use = now;
                let Kind::Txn(c) = &session.kind else {
                    return Err(no_such_transaction_reply(lsid_bytes, txn_number, active));
                };
                c.clone()
            }
            _ => return Err(no_such_transaction_reply(lsid_bytes, txn_number, active)),
        };
        drop(inner);
        let mut t = c.lock().unwrap_or_else(|e| e.into_inner());
        match t.state {
            TxnState::Committed => Err(transaction_committed(txn_number)),
            TxnState::Aborted => Err(transaction_aborted(txn_number)),
            TxnState::InProgress => {
                t.last_use = now;
                drop(t);
                Ok(c)
            }
        }
    }

    /// The session's transaction at `txn_number`, or the reply that says why
    /// there is none.
    fn ending(
        &self,
        lsid_bytes: &[u8],
        txn_number: i64,
    ) -> Result<Arc<Mutex<Transaction>>, Document> {
        let (mut inner, now) = self.locked();
        let Some(session) = inner.sessions.get_mut(lsid_bytes) else {
            return Err(no_such_transaction_reply(lsid_bytes, txn_number, -1));
        };
        session.last_use = now;
        let retryable = matches!(session.kind, Kind::Retryable(_));
        if txn_number < session.active {
            return Err(transaction_too_old(
                lsid_bytes,
                txn_number,
                session.active,
                retryable,
            ));
        }
        match &session.kind {
            Kind::Txn(c) if txn_number == session.active => Ok(c.clone()),
            _ => Err(no_such_transaction_reply(
                lsid_bytes,
                txn_number,
                session.active,
            )),
        }
    }

    /// Commit `(lsid, txnNumber)`. `None` on success (or the repeated commit
    /// of a committed transaction, which drivers send); `Some(reply)` for a
    /// state error.
    pub fn commit(&self, lsid_bytes: &[u8], txn_number: i64) -> Option<Document> {
        let c = match self.ending(lsid_bytes, txn_number) {
            Ok(c) => c,
            Err(reply) => return Some(reply),
        };
        let mut t = c.lock().unwrap_or_else(|e| e.into_inner());
        match t.state {
            TxnState::Committed => None,
            TxnState::Aborted => Some(transaction_aborted(txn_number)),
            TxnState::InProgress => {
                t.last_use = (self.clock)();
                match (self.commit)(&mut t) {
                    Ok(()) => {
                        t.state = TxnState::Committed;
                        self.ended(&t, true);
                        None
                    }
                    // Nothing of it was committed: the transaction is over,
                    // and the client is told why.
                    Err(e) => {
                        t.state = TxnState::Aborted;
                        self.ended(&t, false);
                        let mut reply = crate::util::command_error(e).into_reply();
                        let code = reply.get_i32("code").unwrap_or(0);
                        if crate::failpoints::is_transient_txn_code(code) {
                            add_label_first(&mut reply, TRANSIENT_LABEL);
                        }
                        Some(reply)
                    }
                }
            }
        }
    }

    /// Abort `(lsid, txnNumber)`. `None` on success; `Some(reply)` for a state
    /// error.
    pub fn abort(&self, lsid_bytes: &[u8], txn_number: i64) -> Option<Document> {
        let c = match self.ending(lsid_bytes, txn_number) {
            Ok(c) => c,
            Err(reply) => return Some(reply),
        };
        let mut t = c.lock().unwrap_or_else(|e| e.into_inner());
        match t.state {
            TxnState::Committed => Some(transaction_committed(txn_number)),
            TxnState::Aborted => Some(transaction_aborted(txn_number)),
            TxnState::InProgress => {
                (self.rollback)(&mut t);
                t.state = TxnState::Aborted;
                t.last_use = (self.clock)();
                self.ended(&t, false);
                None
            }
        }
    }

    /// Server-side abort after a failed statement (any statement that fails
    /// while running aborts its transaction). No-op once terminal.
    pub fn abort_in_progress(&self, txn: &Arc<Mutex<Transaction>>) {
        self.abort_locked(txn);
    }

    /// Undo a `startTransaction` whose command turned out not to parse: roll
    /// the transaction back and leave the session as if it had never been
    /// sent.
    pub fn forget_started(&self, lsid_bytes: &[u8], txn: &Arc<Mutex<Transaction>>) {
        {
            let mut t = txn.lock().unwrap_or_else(|e| e.into_inner());
            if t.state == TxnState::InProgress {
                (self.rollback)(&mut t);
                t.state = TxnState::Aborted;
                self.release_locks(&t);
                // Never started, so never ended either.
                self.counts[0].fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let ours = matches!(
            inner.sessions.get(lsid_bytes),
            Some(Session { kind: Kind::Txn(c), .. }) if Arc::ptr_eq(c, txn)
        );
        if ours {
            inner.sessions.remove(lsid_bytes);
        }
    }

    /// Begin, or continue, the retryable write `(lsid, txnNumber)`: the
    /// statements of it that already ran, by statement id. A newer number
    /// aborts the session's open transaction and starts with none.
    pub fn begin_retryable(&self, lsid_bytes: &[u8], txn_number: i64) -> Result<Applied, Document> {
        let (mut inner, now) = self.locked();
        let mut replaced = None;
        if let Some(session) = inner.sessions.get_mut(lsid_bytes) {
            let retryable = matches!(session.kind, Kind::Retryable(_));
            if txn_number < session.active {
                return Err(retryable_too_old(
                    lsid_bytes,
                    txn_number,
                    session.active,
                    retryable,
                ));
            }
            if txn_number == session.active {
                session.last_use = now;
                return match &session.kind {
                    Kind::Retryable(done) => Ok(done.clone()),
                    Kind::Txn(_) => Err(converted_to_transaction()),
                };
            }
            if let Kind::Txn(old) = &session.kind {
                replaced = Some(old.clone());
            }
        }
        inner.sessions.insert(
            lsid_bytes.to_vec(),
            Session {
                active: txn_number,
                kind: Kind::Retryable(Applied::default()),
                last_use: now,
            },
        );
        // A transaction is locked only once the registry is released.
        drop(inner);
        if let Some(old) = replaced {
            self.abort_locked(&old);
        }
        Ok(Applied::default())
    }

    /// Remember what statements of the retryable write `(lsid, txnNumber)`
    /// did. Dropped if the session has moved on to another number.
    pub fn record_statements(
        &self,
        lsid_bytes: &[u8],
        txn_number: i64,
        records: Vec<(i32, StmtRecord)>,
    ) {
        if records.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(Session {
            active,
            kind: Kind::Retryable(done),
            ..
        }) = inner.sessions.get_mut(lsid_bytes)
        {
            if *active == txn_number {
                done.add(records);
            }
        }
    }

    /// `killSessions`: abort the session's in-progress transaction.
    pub fn abort_for_session(&self, lsid_bytes: &[u8]) {
        let txn = match self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sessions
            .get(lsid_bytes)
        {
            Some(Session {
                kind: Kind::Txn(c), ..
            }) => Some(c.clone()),
            _ => None,
        };
        if let Some(c) = txn {
            self.abort_locked(&c);
        }
    }

    /// `killAllSessions` / shutdown: abort everything.
    pub fn abort_all(&self) {
        for c in self.open_transactions() {
            self.abort_locked(&c);
        }
    }

    /// Every session's transaction, taken from the registry so that each is
    /// locked with the registry released: the order every path keeps is a
    /// transaction first, the registry second, never the other way round.
    fn open_transactions(&self) -> Vec<Arc<Mutex<Transaction>>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sessions
            .values()
            .filter_map(|s| match &s.kind {
                Kind::Txn(c) => Some(c.clone()),
                Kind::Retryable(_) => None,
            })
            .collect()
    }

    /// Sessions holding transaction or retryable-write state: mongod's
    /// session catalog.
    pub fn session_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sessions
            .len()
    }

    /// Count of in-progress transactions (test/introspection helper).
    pub fn in_progress_count(&self) -> usize {
        self.open_transactions()
            .iter()
            .filter(|c| c.lock().unwrap_or_else(|e| e.into_inner()).state == TxnState::InProgress)
            .count()
    }

    fn abort_locked(&self, txn: &Arc<Mutex<Transaction>>) {
        let mut t = txn.lock().unwrap_or_else(|e| e.into_inner());
        if t.state == TxnState::InProgress {
            (self.rollback)(&mut t);
            t.state = TxnState::Aborted;
            self.ended(&t, false);
        }
    }

    /// Abort the transactions that have sat idle past their lifetime. The
    /// server calls this about once a second: a transaction nobody ends still
    /// holds its documents, and a writer waiting on one of them is waiting on
    /// this (mongod's periodic `abortExpiredTransactions`).
    pub fn reap(&self) {
        let _ = self.locked();
    }

    /// Abort transactions idle past their lifetime, and, once a minute, forget
    /// sessions idle past theirs. Runs on every access and from `reap`.
    fn prune_locked(&self, inner: &mut Inner, now: f64) {
        let cutoff = now - self.lifetime_seconds();
        for session in inner.sessions.values() {
            if let Kind::Txn(c) = &session.kind {
                // A transaction running a statement is locked, and is not
                // idle: leave it, and do not wait for it holding the registry.
                let Ok(mut t) = c.try_lock() else { continue };
                if t.state == TxnState::InProgress && t.last_use < cutoff {
                    (self.rollback)(&mut t);
                    t.state = TxnState::Aborted;
                    self.ended(&t, false);
                }
            }
        }
        if now - inner.last_sweep >= 60.0 {
            inner.last_sweep = now;
            let stale = now - SESSION_LIFETIME_SECONDS;
            inner.sessions.retain(|_, s| {
                s.last_use >= stale
                    || matches!(&s.kind, Kind::Txn(c)
                        if c.try_lock().map_or(true, |t| t.state == TxnState::InProgress))
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A registry with counting commit/rollback and a settable clock.
    fn reg() -> (Arc<TransactionRegistry>, Arc<AtomicU64>, Arc<AtomicU64>) {
        let commits = Arc::new(AtomicU64::new(0));
        let rollbacks = Arc::new(AtomicU64::new(0));
        let c2 = commits.clone();
        let r2 = rollbacks.clone();
        let r = TransactionRegistry::new(
            Box::new(move |_t| {
                c2.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Box::new(move |_t| {
                r2.fetch_add(1, Ordering::SeqCst);
            }),
            60.0,
            Box::new(|| 0.0),
        );
        (Arc::new(r), commits, rollbacks)
    }

    const LS: &[u8] = &[7u8; 16];

    fn code(reply: &Document) -> i32 {
        reply.get_i32("code").unwrap()
    }

    fn labelled_transient(reply: &Document) -> bool {
        reply.keys().next().map(String::as_str) == Some("errorLabels")
            && reply
                .get_array("errorLabels")
                .unwrap()
                .iter()
                .any(|l| l.as_str() == Some(TRANSIENT_LABEL))
    }

    #[test]
    fn start_then_continue_then_commit() {
        let (r, commits, _) = reg();
        assert!(r.for_statement(LS, 1, true).is_ok());
        assert!(r.for_statement(LS, 1, false).is_ok());
        assert!(r.commit(LS, 1).is_none());
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        // A repeated commit answers ok again and commits nothing.
        assert!(r.commit(LS, 1).is_none());
        assert_eq!(commits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_first_transaction_number_may_be_zero() {
        let (r, _, _) = reg();
        assert!(r.for_statement(LS, 0, true).is_ok());
        assert!(r.commit(LS, 0).is_none());
    }

    #[test]
    fn a_statement_for_no_transaction_names_the_session_and_active_number() {
        let (r, _, _) = reg();
        let err = r.for_statement(LS, 7, false).unwrap_err();
        assert_eq!(code(&err), 251);
        assert!(labelled_transient(&err));
        assert_eq!(
            err.get_str("errmsg").unwrap(),
            "Given transaction number 7 on session 07070707-0707-0707-0707-070707070707 - \
             47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU= -  -  using txnRetryCounter 0 does not \
             match any in-progress transactions. The active transaction number is -1"
        );
    }

    #[test]
    fn after_commit_a_statement_and_an_abort_are_transaction_committed() {
        let (r, _, _) = reg();
        r.for_statement(LS, 1, true).unwrap();
        assert!(r.commit(LS, 1).is_none());
        assert_eq!(code(&r.for_statement(LS, 1, false).unwrap_err()), 256);
        assert_eq!(code(&r.abort(LS, 1).unwrap()), 256);
    }

    #[test]
    fn after_abort_everything_says_aborted_with_the_label() {
        let (r, _, rollbacks) = reg();
        r.for_statement(LS, 1, true).unwrap();
        assert!(r.abort(LS, 1).is_none());
        assert_eq!(rollbacks.load(Ordering::SeqCst), 1);
        for err in [
            r.for_statement(LS, 1, false).unwrap_err(),
            r.commit(LS, 1).unwrap(),
            r.abort(LS, 1).unwrap(),
        ] {
            assert_eq!(code(&err), 251);
            assert!(labelled_transient(&err));
            assert_eq!(
                err.get_str("errmsg").unwrap(),
                "Transaction with { txnNumber: 1 } has been aborted."
            );
        }
    }

    #[test]
    fn starting_at_the_active_number_is_117() {
        let (r, _, _) = reg();
        r.for_statement(LS, 1, true).unwrap();
        assert_eq!(code(&r.for_statement(LS, 1, true).unwrap_err()), 117);
        assert!(r.commit(LS, 1).is_none());
        assert_eq!(code(&r.for_statement(LS, 1, true).unwrap_err()), 117);
    }

    #[test]
    fn an_older_number_is_too_old_for_every_request() {
        let (r, _, _) = reg();
        r.for_statement(LS, 5, true).unwrap();
        for err in [
            r.for_statement(LS, 3, true).unwrap_err(),
            r.for_statement(LS, 3, false).unwrap_err(),
            r.commit(LS, 3).unwrap(),
            r.abort(LS, 3).unwrap(),
            r.begin_retryable(LS, 3).unwrap_err(),
        ] {
            assert_eq!(code(&err), 225);
            assert!(err.get("errorLabels").is_none());
        }
    }

    #[test]
    fn a_higher_start_aborts_the_open_transaction() {
        let (r, _, rollbacks) = reg();
        r.for_statement(LS, 1, true).unwrap();
        r.for_statement(LS, 2, true).unwrap();
        assert_eq!(rollbacks.load(Ordering::SeqCst), 1);
        assert_eq!(r.in_progress_count(), 1);
        assert_eq!(code(&r.commit(LS, 1).unwrap()), 225);
    }

    #[test]
    fn a_failed_statement_aborts() {
        let (r, _, rollbacks) = reg();
        let txn = r.for_statement(LS, 1, true).unwrap();
        r.abort_in_progress(&txn);
        assert_eq!(rollbacks.load(Ordering::SeqCst), 1);
        assert_eq!(code(&r.for_statement(LS, 1, false).unwrap_err()), 251);
    }

    #[test]
    fn a_retryable_write_remembers_its_statements() {
        let (r, _, _) = reg();
        assert!(r.begin_retryable(LS, 4).unwrap().is_empty());
        r.record_statements(LS, 4, vec![(0, StmtRecord::Delete)]);
        assert_eq!(
            r.begin_retryable(LS, 4).unwrap().get(0),
            Some(StmtRecord::Delete)
        );
        // A newer number starts with nothing, and the old one is refused.
        assert!(r.begin_retryable(LS, 5).unwrap().is_empty());
        assert_eq!(code(&r.begin_retryable(LS, 4).unwrap_err()), 225);
        // Another session has its own numbers.
        assert!(r.begin_retryable(&[9u8; 16], 4).unwrap().is_empty());
    }

    #[test]
    fn a_commit_that_fails_in_storage_is_reported_and_ends_the_transaction() {
        let r = TransactionRegistry::new(
            Box::new(|_t| Err(crate::StorageError::WriteConflict)),
            Box::new(|_t| {}),
            60.0,
            Box::new(|| 0.0),
        );
        r.for_statement(LS, 1, true).unwrap();
        let reply = r.commit(LS, 1).expect("the failure is reported");
        assert_eq!(code(&reply), 112);
        assert!(labelled_transient(&reply));
        // Not committed: a second commit does not answer ok.
        assert_eq!(code(&r.commit(LS, 1).unwrap()), 251);
    }

    #[test]
    fn reap_aborts_an_idle_transaction_with_no_other_request() {
        let now = Arc::new(AtomicU64::new(1000));
        let n2 = now.clone();
        let rollbacks = Arc::new(AtomicU64::new(0));
        let r2 = rollbacks.clone();
        let r = TransactionRegistry::new(
            Box::new(|_t| Ok(())),
            Box::new(move |_t| {
                r2.fetch_add(1, Ordering::SeqCst);
            }),
            60.0,
            Box::new(move || n2.load(Ordering::SeqCst) as f64),
        );
        r.for_statement(LS, 1, true).unwrap();
        r.reap();
        assert_eq!(rollbacks.load(Ordering::SeqCst), 0);
        now.fetch_add(61, Ordering::SeqCst);
        r.reap();
        assert_eq!(rollbacks.load(Ordering::SeqCst), 1);
        assert_eq!(r.in_progress_count(), 0);
    }

    #[test]
    fn a_large_insert_batch_is_remembered_as_runs() {
        let (r, _, _) = reg();
        r.begin_retryable(LS, 1).unwrap();
        let batch: Vec<(i32, StmtRecord)> = (0..1000)
            .filter(|i| *i != 500)
            .map(|i| {
                (
                    i,
                    StmtRecord::Insert {
                        id: Some(Bson::Int32(i)),
                    },
                )
            })
            .collect();
        r.record_statements(LS, 1, batch);
        let applied = r.begin_retryable(LS, 1).unwrap();
        assert_eq!(applied.inserted, vec![(0, 500), (501, 499)]);
        assert!(applied.records.is_empty());
        assert!(applied.contains(0) && applied.contains(999) && !applied.contains(500));
        assert!(!applied.contains(1000) && !applied.contains(-1));
        assert_eq!(applied.get(7), Some(StmtRecord::Insert { id: None }));
    }

    #[test]
    fn a_retryable_write_and_a_transaction_share_the_numbers() {
        let (r, _, rollbacks) = reg();
        r.begin_retryable(LS, 4).unwrap();
        assert_eq!(code(&r.for_statement(LS, 4, true).unwrap_err()), 117);
        assert_eq!(code(&r.for_statement(LS, 4, false).unwrap_err()), 251);
        r.for_statement(LS, 5, true).unwrap();
        assert_eq!(code(&r.begin_retryable(LS, 5).unwrap_err()), 217);
        // A newer retryable write aborts the open transaction.
        r.begin_retryable(LS, 6).unwrap();
        assert_eq!(rollbacks.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_idle_transaction_is_aborted_and_an_idle_session_forgotten() {
        let now = Arc::new(AtomicU64::new(1000));
        let n2 = now.clone();
        let r = TransactionRegistry::new(
            Box::new(|_t| Ok(())),
            Box::new(|_t| {}),
            60.0,
            Box::new(move || n2.load(Ordering::SeqCst) as f64),
        );
        r.for_statement(LS, 1, true).unwrap();
        now.fetch_add(61, Ordering::SeqCst);
        assert_eq!(code(&r.for_statement(LS, 1, false).unwrap_err()), 251);
        now.fetch_add(31 * 60, Ordering::SeqCst);
        // Forgotten: the number may be used afresh.
        assert!(r.for_statement(LS, 1, true).is_ok());
    }
}
