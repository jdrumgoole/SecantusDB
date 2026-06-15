# Rust server: multi-document transactions (real WT-backed)

**Decision (2026-06-15, Joe):** implement real WiredTiger-backed transactions on
the Rust server, mirroring the Python server's design — NOT a buffer-and-apply
shim. Transactions are **in scope** (single-node, WT-native; SecantusDB
advertises as a single-node replica-set primary so drivers send them). The
Python server is the oracle: `src/secantus/transactions.py` (the registry) +
`commands.py` (dispatch envelope, commit/abort) + `storage.py`
(`begin_user_transaction` / `use_user_transaction` / `commit_user_transaction`).

Current Rust state: `commitTransaction`/`abortTransaction` are no-op stubs
(`diagnostics.rs`); no `startTransaction`, no session/txn registry, no WT user
transactions. The WT FFI (`secantus-wt`) already exposes
`begin_transaction`/`commit_transaction`/`rollback_transaction`.

Target gauge cluster: `test_transactions_unified.py` (lifecycle, error labels,
abort/commit idempotency, write-conflict), plus snapshot-read-concern-in-txn and
readPreference-in-txn cases. ~15–25 tests once green.

## Phasing

### T1 — Storage user-transactions (the WT plumbing) — ✅ DONE (beta.9)
Landed: `Session: Send` (secantus-wt); a thread-local `ACTIVE_TXN_SESSION` +
`OpSession` accessor + `op_session()` in secantus-storage; `UserTransactionHandle`
(owns a dedicated WT session) + `begin_user_transaction` /
`with_user_transaction` (installs the session for one statement, begins the WT
txn lazily, RAII-restores) / `commit_user_transaction` / `rollback_user_transaction`.
Migrated the 11 transaction-participating CRUD call sites (`insert_one` / `insert`
/ `find_by_id` / `scan_collection` / `replace_by_id` / `delete_by_id` /
`create_collection` / `find_matching_with` / `count_matching` /
`update_matching_core` / `delete_matching`) from `self.conn.open_session()` to
`self.op_session()`; the deliberately cross-thread oplog reads + cluster-time /
meta paths stay on fresh sessions. Validated by gauge non-regression (still
88.4% — `op_session` didn't break CRUD); transaction semantics validated
end-to-end in T2's pymongo e2e. Oplog-during-txn rides the txn session (atomic
with data; a seq gap on abort is the only wart — proper buffering is T3).

### T1 (original detail) — Storage user-transactions (the WT plumbing)
- `secantus-storage`: a `UserTransactionHandle` owning a dedicated WT session
  (NOT the thread-local one — pymongo may send a txn's statements + retryable
  commit on different connections). Methods:
  - `begin_user_transaction() -> UserTransactionHandle` — open a dedicated WT
    session, registered for the close()-sweep; WT `begin_transaction` deferred to
    first use (snapshot pins at first statement).
  - `use_user_transaction(handle, closure)` — install the handle's session as
    this thread's storage session for the closure's duration (begin the WT txn
    lazily), so every existing path (unique probes, index writes, find/update)
    runs inside the WT transaction → read-your-own-writes + pinned snapshot for
    free. Restore the thread session afterward.
  - `commit_user_transaction(handle)` / `rollback_user_transaction(handle)`.
- `secantus-storage-adapter` + the command `Storage` trait: expose the four
  methods (trait defaults: begin returns an opaque handle id, use/commit/rollback
  no-op — so the command-crate fakes still compile).
- Storage-level unit tests: write inside a txn, rollback → invisible; commit →
  visible; read-your-own-writes inside the txn.
- **Oplog buffering deferred to T3** — at first, in-txn writes emit oplog
  normally (change-stream-in-txn atomicity is a refinement).

### T2 — Registry + dispatch + lifecycle — WT-free, easy to unit-test
- New `secantus-commands::transactions` module: port `TxnState` / `Transaction`
  / `TransactionRegistry` (storage-agnostic; commit/rollback injected). Full
  state machine: for_statement (start vs continuation; TransactionTooOld 225,
  TransactionCommitted 256, Location50911, NoSuchTransaction 251 +
  TransientTransactionError), commit (idempotent), abort (no label),
  abort_in_progress, on_retryable_write, lifetime reaper (60s, opportunistic).
- The registry lives on the server (per-server, like CursorRegistry); thread it
  into `CommandContext`.
- Dispatch envelope (mirror `commands.dispatch` ~6082): when `txnNumber` present
  and `autocommit: false` and name ∉ {commit,abort} → resolve via registry; the
  263 `OperationNotSupportedInTransaction` allowlist gate; run the statement
  inside `use_user_transaction`; failed statement → abort + transient label.
  `autocommit` absent → `on_retryable_write`.
- Real `commitTransaction`/`abortTransaction` handlers (replace the stubs).
- WriteConflict → 112 reply with TransientTransactionError label inside a txn.
- Read-concern validation (snapshot → 246 SnapshotUnavailable outside a txn on
  the relevant commands; accepted inside) + apiVersion validation, at dispatch.

### T3 — Refinements
- Oplog buffering during a txn: buffer entries on the handle, flush with one
  shared commit Timestamp + lsid/txnNumber at commit (change-stream-in-txn
  atomicity), mirroring `commit_user_transaction`.
- Snapshot read-concern `atClusterTime` echo on first snapshot read.
- `endSessions`/`killSessions` → abort the session's in-progress txn.
- Edge cases surfaced by the gauge.

## T1 concrete design (Rust storage differs from Python's session model)

The Rust storage opens a **fresh** WT session per public method
(`self.conn.open_session()?`, ~40 call sites) under the global `lock`, unlike
Python's thread-local cached session. So there's no persistent thread session to
swap. Mechanism:

- **`unsafe impl Send for Session`** in `secantus-wt` (a `Session` is a raw
  `*mut WT_SESSION` + a `Drop` that closes it). Sound because a user-txn session
  is only ever used by one thread at a time (the per-`Transaction` mutex in the
  registry serializes statements); WT permits a session to move between threads
  sequentially.
- **A thread-local active-txn session** in `secantus-storage`:
  `thread_local! { static ACTIVE_TXN_SESSION: Cell<*const Session> }` (null when
  not in a txn). `use_user_transaction(handle, closure)` installs the handle's
  session pointer for the closure's duration (begins the WT txn lazily on first
  entry), runs `closure`, then clears it — RAII guard so a panic also clears.
- **`fn op_session(&self) -> Result<OpSession>`**: returns
  `OpSession::Txn(&Session)` (borrowed from the thread-local, no close on drop)
  when one is installed, else `OpSession::Fresh(Session)` (closed on drop).
  `OpSession: Deref<Target = Session>` so existing `session.open_cursor(...)`
  bodies are unchanged.
- **Migrate only the transaction-participating call sites** from
  `self.conn.open_session()?` to `self.op_session()?`: the CRUD write/read paths
  (`insert*`, `update_matching*`, `delete_matching*`, `find_matching*`,
  `count*`, `create_collection`/`_ensure_collection`, unique probes, index
  writes). **Leave fresh** the deliberately-cross-thread paths the CLAUDE.md
  pins: `read_oplog` / `read_preimage` / `oplog_floor_seq` / `find_seq_for_ts`
  (tailable getMore needs a fresh snapshot each poll), `persist_oplog_meta` /
  `current_cluster_time` (must not ride inside the user txn).
- **`begin_user_transaction` / `commit_user_transaction` /
  `rollback_user_transaction`** open/commit/rollback the dedicated session; the
  session is registered for the `close()` sweep so a leaked txn rolls back.

## Notes
- Each phase bumps the Rust crates (beta.N) and runs the rust gauge via the
  background build+gauge sub-agent; merge per-phase when green.
- T1 touches the WT-linked crates → the `SECANTUS_BUILD_STORAGE_ENGINE=ON` build.
- Parity oracle for registry semantics: `transactions.py`'s docstring enumerates
  every state-machine transition with its code — port the unit tests too.
