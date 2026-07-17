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

## Non-finding

The large `__gettimeofday` counts in the insert capture are WiredTiger's
internal service threads computing absolute deadlines for timed condition
waits — wait-adjacent, not our code, not actionable.

## The other axis: concurrency

Unchanged from `docs/concurrency.md`: all writes serialize behind the
single global `Mutex<()>` in `secantus-storage`, giving flat ~0.5×
scaling. The Python server's per-collection-lock split is the ported
design; the `wt_poc` pure-C ceiling (~1.3× aggregate) bounds the prize at
roughly **0.5× → 1.2–1.3× under 4–8 writers (~2.5× throughput)**. The
Rust `wait_for_oplog` loop is already commit-conflict-correct, but the
Python split's lesson list applies (commit-time conflict mapping,
oplog-meta hotspot — the Rust emit currently persists meta under the
global lock, which becomes a hotspot the moment the lock splits).

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
