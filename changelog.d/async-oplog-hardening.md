### Async oplog hardened: transactions can no longer leak ghost events

The Rust server's opt-in async oplog (`RustServer(oplog_async=True)` /
`secantusd-rs --oplog-async`) closed out its prototype caveats. The
important one was a correctness bug the hardening audit caught: a write
inside a multi-document transaction handed its oplog entry to the
background drainer *before* the transaction committed, so a rollback
left a persisted entry for data that never existed — a phantom change
event and a wrong PITR row. Entries now buffer on the transaction handle
and reach the drainer only after the commit succeeds; a rolled-back
transaction leaves no oplog trace.

Two smaller async-mode gaps closed with it. Reading `local.oplog.rs`
now drains the writer's queue first, so a client that just got its
write acknowledged sees the entry in the oplog view — read-your-own-write,
as on mongod. And the opportunistic prune cadence moved from the write
path to the drainers themselves: the old trigger could only prune rows
already persisted, so a lagging drainer queue escaped every sweep and a
burst of writes could leave the oplog over its cap until the next
explicit prune. CI gains an async-oplog lane that runs the whole
storage suite with the drainer pool live.

#### Fixed

- Async oplog: multi-document transaction writes minted + enqueued their
  oplog entries mid-transaction; a rollback persisted a ghost entry
  (phantom change-stream event, wrong PITR). Entries now buffer on the
  transaction handle and are minted + enqueued only after a successful
  commit; rollback / commit-failure / handle drop discard them
  (`crates/secantus-storage/tests/async_txn.rs` pins both directions).
- Async oplog: `local.oplog.rs` reads raced the drainer — an
  acknowledged write's entry could be missing from the view. The view
  read path now flushes the drainer first (no-op in sync mode; skipped
  inside a user transaction, where mongod forbids reading `local`
  anyway).
- Async oplog: the opportunistic prune fired on minted volume but could
  only doom persisted rows, so drainer-queue lag escaped the sweep and
  the counter reset deferred the retry a full interval — an oplog
  temporarily unbounded past `oplog_max_entries` under bursts. The
  cadence now lives with the drainers (triggered as rows land).
- Async oplog: an explicit `prune_oplog` call racing the drainer pruned
  a timing-dependent subset of acknowledged writes (cap-excess rows
  still queued escaped the sweep, shifting the pruned count and the
  resulting oplog floor / PITR segment contents). The public entry
  point now drains the queue first, so explicit prunes
  deterministically cover every acknowledged write.

#### Changed

- `tests/oplog_visibility.rs` pins `oplog_async: Some(false)` (it tests
  the sync in-flight-mint window, which async mode does not have) and
  storage-crate oplog tests pin the async read-after-write contract with
  explicit `flush_oplog()` calls, so the whole suite is meaningful in
  both modes.

#### Added

- CI: an async-oplog parity lane in the `rust-storage` job —
  `cargo test` re-run under `SECANTUS_OPLOG_ASYNC=1` +
  `SECANTUS_OPLOG_NONLOGGED=1` — the stated precondition for the mode
  ever becoming a default.
