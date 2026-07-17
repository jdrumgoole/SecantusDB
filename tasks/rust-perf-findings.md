# Rust server performance profile — findings (2026-07-17)

Status: **measured**. This is the evidence base for the Rust-server
performance work; the numbers below are from `sample`(1) call-tree captures
of a symbolized release build (`CARGO_PROFILE_RELEASE_DEBUG=true`) under
sustained single-client load — five 30-second phases (insert / indexed find
/ full scan / `$group` aggregate / update+delete) driven through `pymongo`
against on-disk WiredTiger, 20-second capture each. Context: the three-way
benchmark has the Rust server at 2.1×–4.5× of mongod per operation
(`docs/benchmark.md`) and flat ~0.5× concurrency scaling
(`docs/concurrency.md`).

## Headline

**Owned-BSON materialization dominates the on-CPU profile in every phase.**
The `bson` crate's serde path (`Document::from_reader` →
`DocumentAccess::advance` / `BsonVisitor::visit_map` →
`indexmap::insert_full`) plus the malloc/realloc/free churn it drives are
the largest CPU consumers across all five workloads. WiredTiger's own
frames (`__wt_row_search`, `__wt_search_insert`, log slot work) are minor
by comparison. The performance gap to mongod is overwhelmingly **above**
the storage engine, in document materialization — as the workload spread
already hinted (inserts 2.1×, scans/aggregates 4.3–4.5×).

## Finding 1 — the scan path materializes every document

`find` (full scan, worker-thread samples): `dispatch` 11,343 →
`find::find` 6,896 → `Storage::find_matching_with` 3,217, of which
`decode_doc` → `bson::Document::from_reader` is **3,211 (99.8%)**. The
storage scan above WiredTiger is, to within noise, *only* BSON
materialization: every stored blob becomes an owned `Document` (an
`IndexMap` with per-key allocations) before matching/projection see it.

## Finding 2 — the reply path materializes them again

`get_more` 4,315 → `util::docs_to_bson` 4,199 → `Document::from_reader`
**3,784**. Cursor batches are re-parsed from bytes into owned `Document`s
purely to build the wire reply, which is then re-serialized. A document
served to a client is fully decoded (at least) twice.

Combined, findings 1+2 put **~65% of the serving path's on-CPU time in
materialization** for scan-shaped workloads.

## Finding 3 — the oplog prune is an O(entire-oplog) full-decode sweep

The single biggest insert-path consumer is not the insert:
`Storage::insert` 14,359 → `emit_oplog` 13,359 → **`prune_oplog_inner`
9,778**, of which `decode_doc` is **9,772**. `prune_oplog_inner`
(`crates/secantus-storage/src/lib.rs`) walks the *whole* oplog table and
fully materializes *every* row to read its `ts` field — and `emit_oplog`
triggers it every 1,000 emits. At the 100k-entry cap that is up to 100k
full document decodes per 1,000 inserted documents, quadratic while the
oplog grows. Two independent fixes, both cheap:

1. Oplog rows are seq-ordered, hence time-ordered: walk from the front and
   **stop at the first row inside the retention window** — O(pruned + 1),
   not O(all).
2. Read `ts` through `bson::RawDocument` field access — no
   materialization. (The cap-based trim needs only a row count, which
   needs no values at all.)

This alone should move the insert workload meaningfully toward mongod
(prune is ~68% of `Storage::insert` time in the capture).

**Status (2026-07-17): shipped** (rust-oplog-hotpath slice) — both fixes as
described (raw `ts` peek via `bson::RawDocument`, early-stop at the first
in-window row, keys-only for the remainder), plus the same raw peek in
`find_seq_for_ts` and a single-`prev()` tail read in oplog-meta recovery.
The slice also took `current_cluster_time`'s per-`hello` meta persist (and
its global-lock hold) off the hot path entirely — the Python endgame ported:
meta persists at close (`Drop`), recovery clamps counters up past the table
maxima and bumps the cluster clock +1s. Semantics pinned by the existing
prune tests plus three new recovery tests in `tests/oplog.rs`; a literal
O(pruned) cost pin isn't practical without decode instrumentation, so the
cost claim rests on the code shape (no decode after the first in-window
row).

## Non-finding

The large `__gettimeofday` counts in the insert capture are WiredTiger's
internal service threads computing absolute deadlines for timed condition
waits — wait-adjacent, not our code, not actionable.

## The other axis: concurrency — the design

`crates/secantus-storage` has 51 `self.lock.lock()` sites, and they include
**pure reads** (`find_by_id`, `scan_collection`) — today concurrent readers
serialize too, not just writers. That splits the work into a cheap step and
a structural one:

1. **Reads off the lock (cheap, first).** WiredTiger MVCC + per-thread
   sessions make lock-free reads safe; drop the mutex from the read-only
   methods (each already opens/uses its own session). No conflict machinery
   needed. Wins on every mixed workload immediately and shrinks writer
   convoy pressure. *(Shipped 2026-07-17, rust-lockfree-reads slice: 19
   read methods unlocked; three write-ordering fixes make the reader
   invariants airtight — diff-based update index maintenance,
   entries-before-registry createIndex, doc-row-first deletes. Residual
   known wobble: rename/dropCollection racing a scan can yield a partial
   result set, the moral equivalent of mongod killing cursors on drop —
   noted in tasks/backlog.md.)*
2. **Per-collection write locks (the port of Python Phase 2).** Registry of
   `(db, coll) → lock` (as `_coll_locks` in Python); the global mutex
   remains only for DDL, multi-collection writes (`$out` / `$merge`,
   `renameCollection`), and registry mutation. `std::Mutex` is not
   reentrant (the code already works around this in `prune`), so either
   restructure call chains to never re-enter or use a reentrant lock for
   the per-collection slots.
   *(Shipped 2026-07-17, rust-coll-locks slice — plus per-statement WT
   snapshot transactions, which the split immediately proved necessary:
   the first stress run caught a lost update from a stale-snapshot
   read-modify-write across autocommit ops. No reentrant locks needed —
   call chains acquire once at the public method.)*
3. **Global counters off the write path.** Oplog seq/ts minting to an
   atomic + micro-lock (Python's `_oplog_seq_lock` shape); stop persisting
   oplog meta under the lock — adopt the Python endgame verbatim: persist
   only on close/prune, recover by table-scan clamp plus the +1s cluster
   clock bump. *(Meta half shipped 2026-07-17 with the finding-3 slice:
   persist at close only, clamp + bump on recovery, `current_cluster_time`
   lock-free of the global mutex. Seq/ts minting already lives under the
   dedicated `oplog` mutex, which the condvar pairing requires — no atomic
   demotion possible or needed.)*
4. **Commit-time conflict handling (port of #444/#447).** Once writers
   overlap, WT can mark a transaction rollback-only with the conflict
   surfacing only at `commit` (bare EINVAL, reason cleared by the
   auto-rollback). Add the Rust equivalent of `_commit_batch_transaction`'s
   mapping plus an unbounded `writeConflictRetry` wrapper with periodic
   warnings. `wait_for_oplog` is already conflict-correct.
   *(Shipped 2026-07-17 with the same slice: EINVAL/WT_ROLLBACK → typed
   WriteConflict at both statement and commit time (the Rust binding
   carries the raw errno, so no message matching); unbounded retry with
   5s-interval warnings outside user transactions; immediate surface
   inside them.)*
5. **Gates.** `tests/test_mongo_server_concurrency.py`'s Rust params (the
   #451 suite: exactly-one-winner, exact counts, typed-errors-only) are the
   correctness harness; `bench.concurrency --server rust` is the measure.
   Do this after the raw-BSON work — critical-section lengths are about to
   change shape, and the split should be tuned against the new profile.

**Expected outcome**: flat 0.5× → ~1.2–1.3× aggregate at 4–8 writers (the
measured `wt_poc` pure-C ceiling), i.e. **~2.5× write throughput under
load**, plus whatever the read-path unlock buys mixed workloads — which is
currently unmeasured because the harness is write-only (add a mixed
readers+writers phase to `bench.concurrency` when step 1 lands).

## Recommended sequence

1. **Prune fix (finding 3)** — small, isolated, immediate insert-path win;
   land with a regression test that pins prune cost to O(pruned).
2. **Raw-BSON serving path (findings 1+2)** — operate on
   `bson::RawDocument` slices through scan → match → project → reply,
   materializing only where an operator genuinely needs an owned value.
   Biggest single-client lever (~65% of scan-path CPU is at stake);
   largest surface, needs the parity suites at every step.
3. **Lock split (concurrency)** — independent axis, design ported from the
   Python server; do after (2) so the split isn't tuned around
   materialization costs that are about to change shape.

Raw capture files: `sample-{insert,find_range,find_all,aggregate,update_delete}.txt`
(session scratchpad; regenerate with `bench/profile_driver`-style loops +
`sample <pid> 20` against a `CARGO_PROFILE_RELEASE_DEBUG=true` build).
