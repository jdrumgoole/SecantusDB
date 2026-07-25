# Async / decoupled oplog — prototype (2026-07-25)

Status: **working prototype, opt-in, measured**. Gated behind
`SECANTUS_OPLOG_ASYNC=1` (default off = the synchronous, atomic oplog). This is the
"transformative lever" flagged in `rust-perf-findings.md` Finding 5.

## Why

Finding 5 established that the shared oplog is the *sole* concurrency ceiling for
per-collection write workloads: with the oplog off the Rust server scales ~4.8× at
8 writers (≈ mongod's 4.67×), but every CRUD write appends a full oplog entry to
shared oplog btrees *inside its own transaction*, so writers contend on those
btrees / the WAL / eviction and 8-writer scaling collapses to ~1.8×. Non-logging
the oplog tables recovered only ~+14–24% (the WAL log was a minor slice). The real
fix is to take the oplog write **off the writer's critical path** entirely.

## Design

The oplog write no longer happens in the writer's transaction. Instead:

1. **Buffer, don't write.** In async mode `emit_oplog_entries` pushes the entries
   (still un-minted — no seq/ts yet) into a thread-local `PENDING_OPLOG` instead of
   writing them to WiredTiger.
2. **Mint + enqueue AFTER the data commits.** `with_statement_txn`, on a *successful*
   commit, mints the seq range + timestamps, builds the entry blobs, and sends them
   as one `DrainBatch` to a background drainer. On rollback / write-conflict-retry it
   *clears* the buffer instead — so a rolled-back write never mints a seq (**no gaps**)
   and never enqueues (**no duplicate change events**). This post-commit mint is the
   correctness lynchpin. (An emit outside a wrapped statement — e.g. the noop
   heartbeat — buffers and drains itself immediately, so it too mints-then-enqueues
   and never sync-writes a seq the drainer can't see.)
3. **Drainer persists in seq order.** A single background thread owns its own WT
   session, receives batches, buffers them in a `BTreeMap` keyed by `start_seq`, and
   writes every *contiguous* run (each batch in one WT transaction), advancing a
   `written_seq` watermark under the `oplog` mutex and notifying `oplog_cv`. Concurrent
   writers enqueue out of order; the drainer reorders, so `written_seq` only ever
   advances over a gapless prefix.
4. **Tailers wait on `written_seq`, not `next_seq`.** `wait_for_oplog` blocks on the
   drainer's durable watermark, so a change-stream getMore never reads past an entry
   the drainer has not yet written. (`flush_oplog()` blocks until `written_seq`
   catches up to the minted tail — read-after-write visibility for a test / backup /
   consistency checkpoint.)
5. **Clean shutdown flushes.** `Storage::drop` sends `Shutdown` and joins the drainer
   *before* persisting oplog meta + the close checkpoint, so a clean close checkpoints
   a complete oplog (clean-restart durability preserved).

## Measured (8 writers, per-writer collections, embedded Rust, interleaved,
## stable regime — all reps tight)

| arm | docs/s | scaling vs 1-writer (~25.7k) | penalty vs off |
|---|---:|:---:|:---:|
| sync oplog (current default) | 46,677 | 1.8× | 3.08× |
| **async oplog (prototype)** | **102,832** | **4.0×** | **1.40×** |
| no oplog (ceiling) | 144,375 | 5.6× | 1.00× |

**async is 2.20× the synchronous throughput at 8 writers** and moves scaling from
1.8× to 4.0× — approaching mongod's 4.67× and 71% of the no-oplog ceiling.
(`scratchpad/conc_async_ab.py`.)

## Correctness validated

- **Sync mode (default) unchanged:** the whole storage crate suite passes (oplog 21,
  changestreams 13, crud 7, batch_insert 6, concurrent_writes 7, …) — the async path
  is dormant unless the env flag is set.
- **Async single-threaded:** end-to-end pymongo change streams produce correct
  `fullDocument` / `updateDescription` / replace / delete events
  (`scratchpad/cs_smoke.py` under the flag).
- **Async under concurrency:** 6 writers × 500 inserts, a cluster-wide change stream
  observes **exactly 3000 events — 0 duplicates, 0 missing, every insert exactly
  once**, reproducibly (`scratchpad/async_cs_concurrent.py`). This is the drainer's
  reorder + contiguous-`written_seq` invariant holding under load.

## Durability trade

The oplog is no longer atomic with the data and is no longer as crash-durable: a
**hard crash** loses oplog entries the drainer had not yet written (an in-memory
queue window, typically small — the drainer keeps up). **Data stays fully logged /
durable**; a **clean close** flushes the drainer before checkpointing, so a clean
restart preserves the whole oplog (change-stream resume / PITR intact). This is the
same class of trade as the non-logged-oplog option, and strictly better than the
benchmark's standalone mongod (no oplog at all). Appropriate for SecantusDB's
ephemeral-test audience; it is opt-in precisely because it changes a durability
property.

## Remaining work before this is more than a prototype

- **Backpressure.** The drainer channel is unbounded — a sustained writer burst that
  outpaces the drainer grows memory without bound. Add a bounded queue (blocking
  enqueue) or a high-water-mark that briefly stalls writers.
- **Config surface.** Currently an env flag (`SECANTUS_OPLOG_ASYNC`). Promote to an
  explicit `RustServer(oplog_async=…)` / `secantusd-rs --async-oplog` option with the
  durability trade documented at the call site.
- **Read-after-write oplog reads.** Direct `read_oplog` / `oplog_tail_seq` callers
  that assume synchronous visibility (several unit tests, some admin paths) must call
  `flush_oplog()` first in async mode; today they race the drainer (the async-mode
  unit-test "failures" are exactly this — not bugs, but a semantic change to pin
  down).
- **DDL that manages its own transaction** (outside `with_statement_txn`) would drain
  immediately, i.e. enqueue before its own commit — audit those paths (rare; not hit
  by CRUD or the gauges) and route them through the buffered path or keep them sync.
- **`prune_oplog` under async:** opportunistic prune no longer fires from the write
  path (it lived in the synchronous emit); move it into the drainer or a timer.
- **Gauge + parity** under the flag, and a `SECANTUS_OPLOG_ASYNC=1` CI lane, before
  it could ever become a default.
