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
## clean orphan-guarded runs — all reps tight)

| async-ON config | docs/s | ×sync (~46–53k) | note |
|---|---:|:---:|---|
| **128 MB cap — bounded / SUSTAINABLE** | **~71.4k** | **1.35×** | the real number |
| unbounded cap — transient | ~103k | 2.28× | **not sustainable** (queue grows toward OOM) |
| no oplog (ceiling) | ~144–150k | 3.1× | data-only scaling |

**The sustainable async win is ~1.35× at 8 writers** (single drainer + the 128 MB
backpressure cap). The earlier "2.2× / 4.0×-scaling" reading was the *unbounded*
transient: with no cap, writers produce ~103k/s while the **single drainer thread
sustains only ~71k/s**, so the queue grows ~30k entries/s (toward OOM over a long
run) — the 103k is writers outrunning the drainer, not a sustainable rate. Proven
by an A/B on the cap: a 10 GB (effectively unbounded) cap reproduces ~103k, the
128 MB cap settles at ~71k, and the backpressure mutex is *not* the cost (the
unbounded run has the same mutex). So **the drainer's single-thread write
throughput (~71k/s) is the new bottleneck** — see "Next lever" below.
(`scratchpad/conc_async_ab.py`; the cap is `SECANTUS_OPLOG_ASYNC_CAP_BYTES`.)

## Next lever: a parallel drainer

To make the higher rate *sustainable* the drainer must out-run the writers. A
single thread caps oplog writes at ~71k/s; a **pool of drainers** (e.g. one per
oplog shard, so writes spread across cores as the sharding intends) would raise
aggregate drain throughput toward the ~103k writer rate — at which point the queue
stays small and async approaches the no-oplog ceiling *sustainably*. The added
complexity is the `written_seq` watermark: with concurrent drainers, completion is
out of order, so the contiguous-prefix computation moves from the current
single-thread `next_expected` into a shared completion tracker (a BTreeMap of
finished seq ranges whose contiguous prefix is the watermark). Flagged, not built.

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

## Backpressure (implemented)

The drainer is fed through a **byte budget** (`Backpressure`, same shape as the
server's `AllocBudget`): a just-committed writer reserves its batch's bytes before
enqueuing and blocks while more than `SECANTUS_OPLOG_ASYNC_CAP_BYTES` (default
128 MB) is queued-but-not-yet-persisted (channel + reorder buffer); the drainer
releases each batch's reservation after it lands. A lone batch larger than the cap
still proceeds (never deadlocks). So a sustained writer burst that outpaces the
drainer blocks at the enqueue point instead of growing memory without bound.
Validated: with an 8 KB cap, 4 writers × 400 inserts (~720 KB of oplog) are all
delivered to a change stream exactly once — zero loss, no deadlock — the queue
never exceeding 8 KB (`scratchpad/conc_backpressure.py`). At the default 128 MB cap
the 8-writer async rate settles at the drainer's sustainable ~71k/s (1.35× sync);
this is not backpressure *overhead* (an unbounded cap has the same mutex and hits
~103k) but backpressure correctly bounding the queue to the single drainer's real
throughput — see the Measured section.

## Remaining work before this is more than a prototype

- **Config surface.** Currently an env flag (`SECANTUS_OPLOG_ASYNC`) + an env cap
  override (`SECANTUS_OPLOG_ASYNC_CAP_BYTES`). Promote to an
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
