# Rust server: multi-document transactions (real WT-backed)

> **DELIVERED (audited 2026-08-20).** Its "Current Rust state" paragraph — no-op
> `commitTransaction`/`abortTransaction` stubs, no `startTransaction`, no
> session/txn registry, no WT user transactions — is now false in every clause.
> `crates/secantus-commands/src/transactions.rs` exists, `startTransaction` is
> dispatched (`commands/src/lib.rs`, `admin.rs`), and `secantus-storage` tracks
> user transactions (`in_user_txn`, lib.rs:3765). The Rust server has since had
> transaction *bug fixes* on top, e.g. #809's cross-transaction unique-index
> enforcement — see the `[x]` entries under "Multi-document transaction
> limitations" in `tasks/backlog.md`, which are the current record.

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

### T2 part 1 — Registry — ✅ DONE (merged in beta.9)
`secantus-commands::transactions` ported from `transactions.py` (TxnState, error
replies 251/256/225/50911, `TransactionRegistry` with for_statement / commit /
abort / abort_in_progress / on_retryable_write / lifetime reaper; storage-agnostic
injected commit/rollback; injectable clock). 9 unit tests pin every transition.
Inert until the dispatch wiring below.

### T2 part 2 — Dispatch integration — ✅ DONE (beta.10)
All 8 steps landed: trait methods (begin/run_in/commit/rollback_user_transaction
with defaults) + WT adapter overrides (downcast handle → storage
with/commit/rollback); `CommandContext.transactions` + builder; server-side
registry (commit/rollback close over storage, `now_secs_f64` clock) wired into
every `make_context`; the `run_with_txn_envelope` dispatch block (resolve via
registry, 263 allowlist gate, run inside `run_in_user_transaction`, failed-stmt
abort + `TransientTransactionError` label, retryable-write `on_retryable_write`);
real `commitTransaction`/`abortTransaction` handlers; `validate_read_concern`
accepts `snapshot` inside a txn / on RS reads (else 246). Validated by
`/tmp/txn_check.py` + the gauge (`test_transactions_unified.py` cluster).

### T2 part 2 — Dispatch integration (original detail)
Concrete steps:
1. **`Storage` trait** (`secantus-commands/src/storage.rs`): add
   `begin_user_transaction(&self) -> Result<Box<dyn Any + Send>, StorageError>`
   (default: `Ok(Box::new(()))`), `run_in_user_transaction(&self, handle: &mut
   (dyn Any+Send), f: &mut dyn FnMut() -> HandlerResult) -> HandlerResult`
   (default: `f()`), `commit_user_transaction` / `rollback_user_transaction(&self,
   handle: &mut (dyn Any+Send)) -> Result<(), StorageError>` (default `Ok(())`).
   Defaults keep the command-crate fakes compiling AND make the state machine
   work (lifecycle/error-label tests) before real WT isolation lands.
2. **WT adapter** (`secantus-storage-adapter`): override the four — downcast the
   handle to `secantus_storage::UserTransactionHandle`; `run_in_user_transaction`
   → `storage.with_user_transaction(handle, f)`.
3. **`CommandContext`**: add `transactions: Option<Arc<TransactionRegistry>>` +
   `with_transactions` builder + accessor (mirror `cursors`).
4. **Server wiring** (`secantus-server` / `-server-py`): create one
   `TransactionRegistry` per server with commit/rollback callbacks closing over
   the storage `Arc` (`|txn| storage.commit_user_transaction(txn.handle…)`); pass
   it into each per-request `CommandContext`.
5. **Dispatch envelope** (`lib.rs::dispatch_inner`, around the handler call):
   when `txnNumber` present + `autocommit:false` + name ∉ {commit,abort} →
   `registry.for_statement(lsid, txn, start)`; on error reply return it; else
   create the handle lazily (`begin_user_transaction`) if `txn.handle` is None,
   run the handler via `storage.run_in_user_transaction(handle, &mut || handler)`,
   then on failure `registry.abort_in_progress` + add `TransientTransactionError`
   label for transient codes. `autocommit` absent → `on_retryable_write`. Borrow:
   clone `ctx.storage` Arc so the closure can still borrow `ctx`.
6. **commit/abort handlers**: replace `diagnostics::ok_transaction` with real
   `commitTransaction`/`abortTransaction` that pull `(lsid, txnNumber)` from the
   doc and call `registry.commit` / `registry.abort`.
7. **Read-concern**: `validate_read_concern` rejects `snapshot` (246
   SnapshotUnavailable) outside a txn on the relevant commands; accept inside.
8. **WriteConflict**: a WT-rollback from a write path inside a txn → 112
   `WriteConflict` + `TransientTransactionError` label.
Validate: a pymongo e2e (commit persists, abort rolls back, read-your-own-writes,
NoSuchTransaction/TransactionCommitted lifecycle) + the rust gauge (expect the
`test_transactions_unified.py` cluster to move). Bump beta.10.

### T3a — WriteConflict (112) mapping — ✅ DONE (beta.11)
A `WT_ROLLBACK` (lost write-conflict race) was falling through `From<WtError>` →
`StorageError::Wt` → adapter `InternalError(1)`. Now: storage `From<WtError>`
detects `is_rollback()` → `StorageError::WriteConflict`; adapter maps it to the
command-crate `StorageError::WriteConflict`; `command_error` → `112 WriteConflict`
(+ `code_name_for(112)`); the `update`/`delete` handlers route it command-level
(`insert` already did) so the dispatch envelope's `finish_txn_statement` attaches
`TransientTransactionError` (112 is in the transient set). Recovers
`test_write_conflict_abort`/`commit`. Validated by `/tmp/wc_check.py` (112 +
transient label, winner commits). **Note:** non-txn writes still surface 112
without the Python server's transparent retry wrapper — a separate enhancement.

### T3 — Refinements (deferred — topology-inherent / niche)
The remaining txn-cluster fails are topology-inherent (`secondary`/`nearest`
readPref on a single-node RS — no secondary to select) or change-stream OOM
(`test_split_large_change`), not transaction-logic gaps. Oplog-buffering
(change-stream-in-txn atomicity) is correctness polish with no current gauge
lever. Original refinement list:
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
