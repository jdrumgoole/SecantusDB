# Async-oplog hardening (toward default-eligibility)

Branch: `async-oplog-hardening` · Worktree: `../SecantusDB-async-oplog-hardening`

Closes the three "Still open" items in `tasks/rust-async-oplog-prototype.md`:
read-after-write semantics, the DDL/own-transaction emit audit, and the async CI
lane. Ground truth: `SECANTUS_OPLOG_ASYNC=1 cargo test --no-fail-fast` on
`secantus-storage` = **38 failures across 8 targets** (lib 3, batch_insert 1,
changestreams 6, lifecycle 3, oplog 15, oplog_visibility 5, options 2, write 3).

## Findings

1. **User-transaction ghost entries (real bug, the audit's catch).**
   `with_statement_txn` early-returns for `OpSession::Txn` without setting
   `IN_ASYNC_STMT`, so a write inside a multi-document transaction buffers its
   entries and — the flag being false — `emit_oplog_entries` immediately mints
   and enqueues them to the drainer, BEFORE the user transaction commits. A
   rollback leaves a persisted oplog entry for data that never committed (ghost
   change event / wrong PITR); even on commit the event can become visible
   before the data does.
2. **DDL paths are fine.** `create_collection` / `drop_collection` / etc. run
   autocommit WT ops then emit; the self-draining emit (outside a wrapped
   statement) mints after those ops are already committed. No enqueue-before-
   commit there.
3. **`local.oplog.rs` view lags acked writes.** `find_oplog_rs` reads only up
   to `written_seq`; right after an acked write the entry may still be queued.
   mongod semantics: an acked w:1 write is in the oplog when the reply returns.
4. **Opportunistic prune can't do its job from the writer thread.** In async
   mode `drain_pending_oplog` triggers the sweep on minted volume, but the
   sweep dooms only *persisted* rows — queue lag escapes, and `emit_count`
   resets anyway, deferring retry a full interval. The oplog is unbounded in
   the stop-writing tail case.
5. **Mode-specific tests inherit ambient env.** `oplog_visibility.rs` tests
   sync-only in-flight-window semantics; `options.rs` asserts env-off
   behaviour; both fail under the forced env rather than pinning their mode.
   (`StorageOptions.oplog_async: Some(false)` beats the env var — the pin
   exists.)
6. **Read-after-write tests race the drainer.** The bulk of the 38: write →
   `read_oplog` immediately. The documented async contract is "call
   `flush_oplog()` first"; the tests should pin that contract (flush is
   correct and cheap in sync mode too).

## Changes

- **F1 — user-txn buffering.** `with_user_transaction` sets `IN_ASYNC_STMT`
  for the closure (guard-restored) so emits buffer; a Drop guard harvests
  `PENDING_OPLOG` onto the handle (new field `pending_async`).
  `commit_user_transaction` mints + enqueues the harvested entries only after
  the WT commit succeeds (refactor `drain_pending_oplog` → shared
  `mint_and_enqueue`); rollback / handle Drop discard them. Tests: async-pinned
  user txn commit → flush → exactly-once entry; rollback → flush → no entry.
- **F2 — view flush.** `find_oplog_rs` (find + count both route through it)
  calls `flush_oplog()` first in async mode.
- **F3 — drainer-side prune cadence.** Track persisted volume in `OplogState`
  (bumped where `written_seq` advances, under the oplog mutex); when it
  crosses `OPLOG_PRUNE_INTERVAL` the crossing drainer runs the sweep.
  Extract `prune_oplog_inner` + `archive_doomed_oplog` into free functions
  over a shared `PruneCtx` (prune lock, settings, archive dir, stable-seq
  clamp pieces) reachable from `DrainerShared`; Storage keeps thin wrappers.
  Remove the writer-side async trigger in `drain_pending_oplog` (sync path
  unchanged). Test gains `flush_oplog()` before counting.
- **F4 — test pins.** flush-before-read in the read-after-write tests;
  `oplog_async: Some(false)` pins in `oplog_visibility.rs`; ambient-env skip
  guards in the two `options.rs` tests (+ `stable_checkpoint_marker` — inspect
  its failure first).
- **F5 — CI lane.** `SECANTUS_OPLOG_ASYNC=1` (stacked with
  `SECANTUS_OPLOG_NONLOGGED=1`) cargo-test lane for `secantus-storage` in
  `test.yml`, mirroring the FORCE_DURABLE lane pattern.
- Docs: update `tasks/rust-async-oplog-prototype.md` "Still open",
  `docs/concurrency.md` caveats, `changelog.d/` fragment.

## Exit criteria

- `SECANTUS_OPLOG_ASYNC=1 cargo test` green on secantus-storage; default-mode
  `cargo test` green; fmt + clippy green (crate-local — WT-linked crate).
- New user-txn ghost-entry regression tests in both modes.
- CI lane added and green.
