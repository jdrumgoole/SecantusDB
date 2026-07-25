# Rust server performance profile — findings (2026-07-17)

Status: **measured**. This is the evidence base for the Rust-server
performance work; the numbers below are from `sample`(1) call-tree captures
of a symbolized release build (`CARGO_PROFILE_RELEASE_DEBUG=true`) under
sustained single-client load — five 30-second phases (insert / indexed find
/ full scan / `$group` aggregate / update+delete) driven through `pymongo`
against on-disk WiredTiger, 20-second capture each. Context: the three-way
benchmark has the Rust server at 2.1×–4.5× of mongod per operation
(`docs/benchmark.md`) and — pre-RecordId — flat ~0.5× concurrency scaling
(re-baselined 2026-07-24 to positive-to-4-writers-then-a-cliff; see the bottom
of this doc)
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

**Finding 1 fixed for the match path — Phase 2 shipped (2026-07-19,
`rawbson-scan` branch).** `secantus_core::query::matches_raw` filters over
`bson::RawDocument`, decoding only the fields the filter reaches;
`find_matching_with` and `count_matching` now use it, so a rejected candidate
is never fully decoded. Measured on a 5000-row collection scan, wide docs
(11 fields), selective filter (one field, rejects 99.8%): **~84 → ~237
scans/s, ≈2.8×**. Pinned bool-for-bool to the owned matcher (and pure Python)
across the whole query parity corpus. Still materializing (later phases): the
sort-key extraction of a post-sorted find (decodes matched rows only), and the
update/delete candidate scans (a matched row is needed for the write).

**Return path (projection) — Phase 3 shipped (2026-07-19, `rawbson-proj`
branch).** A `find` *with* a projection decoded every returned document before
projecting. `secantus_core::projection::apply_projection_raw` now handles the
common shape — a pure top-level inclusion (`{a:1,b:1}`, optionally `_id:0`) —
by decoding only the included fields off the raw document, byte-identical to
`apply_projection` (`_id` first, then included fields sorted). Everything else
(exclusion, dotted, `$slice`/`$elemMatch`/`$meta`, positional, mixed) falls
back to the full projection. Measured on a 5000-row scan of wide docs (12
fields) projecting 2: **~44 → ~87 scans/s, ≈2.0×**. Still materializing: the
exclusion / dotted / operator projection shapes (fall back), and the raw
document that a non-projected find *returns* is spliced onto the wire already
(Phase 1) so it's never decoded server-side — the remaining server-side read
materialization is now just those fallback projection shapes and the
aggregation pipeline (Finding 4 territory).

**Write-path match + aggregation prefix shipped (2026-07-19).** The
`update` / `delete` candidate scans also match over raw BSON now
(`rawbson-write` branch) — rejected candidates skip the full decode, only a
matched row is decoded for the write — measured **≈4.1×** on a selective
`updateMany` COLLSCAN of wide docs. And an aggregation pipeline's leading
`$skip` / `$limit` / `$match` prefix is reduced over the raw fetched blobs
before `decode_docs` (`rawbson-agg` branch, `reduce_raw_prefix`), so the
heavier stages decode only the survivors — measured **≈4.3×** on
`[{$limit:50},{$group}]` over 5000 wide docs. Every scan-*match* path (find /
count / update / delete) is now raw. **Still fully materializing:** the
heavier aggregation stages themselves (`$group` / `$sort` / computed
`$project` / `$unwind`) — each would need a raw stage matching its owned
semantics, or a streaming/slot execution model. That (plus the fallback
projection shapes) is the remaining server-side read materialization.

## Finding 2 — the reply path materializes them again

`get_more` 4,315 → `util::docs_to_bson` 4,199 → `Document::from_reader`
**3,784**. Cursor batches are re-parsed from bytes into owned `Document`s
purely to build the wire reply, which is then re-serialized. A document
served to a client is fully decoded (at least) twice.

Combined, findings 1+2 put **~65% of the serving path's on-CPU time in
materialization** for scan-shaped workloads.

**Finding 2 fixed — Phase 1 shipped (2026-07-19, `rawbson-reply` branch).**
`secantus_wire::encode_cursor_reply` splices the pre-encoded document blobs
straight into `cursor.firstBatch` / `cursor.nextBatch` (RawArrayBuf memcpy,
no decode), and the no-projection `find` + non-tailable `getMore` handlers
hand their batch to the server as raw blobs (`CommandContext::pending_batch`)
instead of an owned `Bson::Array`. Byte-identical to the old reply (unit
test), so no driver-visible change. Measured before/after (baseline
`a2a61595` vs the branch, embedded server, 2000×~220B docs, client-observed
throughput — the pymongo client-side decode cost is the *same* on both sides,
so the server-side gain is larger than these end-to-end numbers show):

| Read workload | Before | After | Speedup |
|---|---:|---:|---:|
| Single-batch (large `firstBatch`, no getMore) | ~511k docs/s | ~673k docs/s | **1.32×** |
| getMore-heavy (batchSize 50, ~40 round-trips/scan) | ~273k docs/s | ~305k docs/s | **1.12×** |

The firstBatch case wins most (a big batch decodes the most documents); the
getMore case gains less because per-batch round-trip overhead dilutes the
per-document decode saving. Still deferred (later phases): projected `find`,
the tailable/change-stream reply, aggregate `firstBatch`, and exhaust-cursor
streaming — plus Finding 1 (the *scan-side* materialization), the larger
remaining lever.

## Post-raw-BSON three-way re-profile (2026-07-19, pinned `9f87edf3`)

After Phases 1–5 landed (reply splice #545, raw scan match #551, raw
projection #556, raw update/delete match #560, aggregation prefix #566), the
end-to-end three-way benchmark (`./inv compare-servers --count 10000 --reps 5`,
on-disk WiredTiger via pymongo, median of 5, mongod baseline spawned from
`/opt/homebrew/bin/mongod`). Extension rebuilt against the pin;
`git rev-parse HEAD` verified unchanged before and after the run.

| Workload | mongod | SecantusDB-rs | ×mongod | SecantusDB (Py) | ×mongod |
|---|---:|---:|:---:|---:|:---:|
| insert | 57.29 ms | 86.82 ms | 1.5× | 335.59 ms | 5.9× |
| find_indexed_range | 4.39 ms | 6.49 ms | 1.5× | 29.14 ms | 6.6× |
| find_all (full scan) | 7.73 ms | 18.90 ms | **2.4×** | 95.19 ms | 12.3× |
| update_many_half | 34.84 ms | 52.93 ms | 1.5× | 499.43 ms | 14.3× |
| **aggregate_group** | 5.72 ms | 17.81 ms | **3.1×** | 136.08 ms | 23.8× |
| delete_many_half | 20.94 ms | 35.64 ms | 1.7× | 314.39 ms | 15.0× |

**The raw-BSON roadmap closed the bulk of the gap.** The pre-work baseline
was 2.1×–4.5× of mongod across the board (`docs/benchmark.md`); four of six
workloads now sit at ~1.5×, near the WT/wire floor where further raw-BSON
serving work has diminishing returns.

**The residual gap is concentrated in two workloads, and it is materialization
the raw-BSON phases deliberately do not touch:**

- **`aggregate_group` — 3.1× (the standout).** This is `$group`, a
  *materializing* stage. Phase 5's `reduce_raw_prefix` only streams the
  *pass-through* prefix (`$skip`/`$limit`/`$match`) over raw blobs and hands
  the survivors to `decode_docs`; the group itself still builds a fully-typed
  `Vec<Document>` and threads owned `Bson` between accumulators. Profiled cause
  is Finding 1's materialization surviving into the heavy stage, exactly as
  noted at the end of the Finding-1 write-path block above.
- **`find_all` — 2.4×.** Full scan. Phases 1+2 already made both the scan-match
  and the reply splice raw, so this is closer to the cursor-batching / wire
  floor than to a decode hotspot — a smaller, separate lever (batch cursor
  efficiency) than the aggregate gap.

**Conclusion → Phase 6 (streaming / slot-based aggregation) is the next lever.**
The profile confirms empirically that the remaining gap lives in aggregate
materialization, not projection or planning. Re-measure `find_all` after
Phase 6 changes the aggregate materialization costs before deciding whether
the cursor-batch lever is worth a separate slice. Scoping:
`tasks/rust-phase6-streaming-agg-scoping.md`.

**Phase 6a shipped — `$group` field-reference pushdown (`rawbson-group`
branch).** `secantus_core::referenced_top_level_fields` walks a `$group` spec
and returns the top-level fields its `_id` + accumulators read (bailing on
`$$ROOT`/`$$CURRENT`, computed-field access, and non-simple accumulators like
`$top`/`$topN` whose `sortBy` names a field by bare key). When the first
heavier stage of a pipeline is such a `$group`, the command layer decodes only
those fields from each survivor (`decode_docs_minimal` over `bson::RawDocument`)
and feeds the **unchanged** `group_stage` — so `eval("$k", minimal)` is
byte-identical to `eval("$k", full)`. Pinned by `test_rust_group_field_pushdown`
(`apply_pipeline(minimal) == apply_pipeline(full)` over curated + fuzz, plus the
exact field-sets and bail set).

Measured **same-environment A/B in the worktree** (both server builds and both
`compare-servers --count 10000 --reps 5` runs in `SecantusDB-rawbson-group`, so
the `×mongod` is self-normalizing and the unaffected controls cancel run-to-run
variance):

| Workload | Baseline (rs ×mongod) | 6a (rs ×mongod) | Note |
|---|:---:|:---:|---|
| **aggregate_group** | 3.1× (18.10 ms) | **2.4× (13.85 ms)** | **≈1.31× faster** — the target |
| find_all | 2.7× | 3.1× | control (untouched; run variance) |
| insert | 1.5× | 1.6× | control |
| find_indexed_range | 1.6× | 1.7× | control |
| update_many_half | 1.6× | 1.7× | control |
| delete_many_half | 1.7× | 1.7× | control |

So `$group` moved **3.1× → 2.4×** of mongod (~24% faster) on the benchmark's
7-field documents reading 2 fields — proportional to (doc width)/(fields read),
as scoped; wider documents win more. This is the raw-decode lever (6a); the
heavier `$group`/`$sort` execution model (streaming, 6b) stays deferred behind a
multi-stage workload measurement, per the scoping doc.

## Multi-stage aggregate re-profile — is 6b (streaming) justified? (2026-07-19, pinned `73b1790b`)

The scoping doc's precondition for committing to 6b: add a **multi-stage**
aggregate workload to the benchmark (the single-stage `aggregate_group` can't
show inter-stage buffering) and measure whether it's a real gap. Added
`aggregate_multistage` to `bench/compare_servers.py`: `[{$match: active}, {$unwind:
"$tags"}, {$group: {_id: "$tags", total: {$sum: "$v"}, n: {$sum: 1}}}, {$sort:
{total: -1}}]` over a separate 3-element-array collection (populated untimed), so
`$unwind` fans each survivor out ~3× and ~15k documents flow into `$group`. The
leading `$match` lifts into the fetch; the first *heavier* stage is `$unwind`, so
6a's field pushdown does **not** apply — this measures the multi-stage path
cleanly. Pinned worktree at `73b1790b` (origin/main, with 6a); `git rev-parse
HEAD` unchanged across both runs.

**The write-workload `×mongod` this session is contaminated** — the machine was
under sustained load (insert/update/delete showed mongod at 3–4× its usual time,
Rust nominally "beating" mongod, which is impossible). Disregard the write rows.
The **aggregate** rows were stable and sensible across two independent runs and
are the reliable signal:

| Aggregate workload | mongod | Rust | ×mongod (2 runs) |
|---|---:|---:|:---:|
| `aggregate_group` (single-stage) | 6.2–6.3 ms | 14.0–14.2 ms | **2.2–2.3×** |
| `aggregate_multistage` (`$unwind`→`$group`→`$sort`) | 7.7 ms | 20.6–21.1 ms | **2.7×** |

**The delta-of-deltas isolates the inter-stage cost.** Adding the three stages
costs **mongod +1.5 ms** (6.2 → 7.7) but **Rust +6.5 ms** (14.0 → 20.6). So
~5 ms of the ~20 ms multi-stage time (≈25%) is Rust paying to fully materialize
the ~15k-document `$unwind→$group` intermediate as an owned `Vec<Document>` where
mongod streams it. This is exactly the cost 6b (streaming / slot execution)
would target, and the multi-stage pipeline does sit worse relative to mongod
(2.7×) than the single-stage group (2.2×).

**Conclusion: 6b's target is real but modest — recommend deferring it.** The
inter-stage buffering gap is measurable (~5 ms, ~0.5× of ratio) but small in
absolute terms, and 6b is the roadmap's largest lift (a rewrite of
`secantus_core::aggregate`'s execution model behind `apply_pipeline` + a full
parity re-pin of every stage's ordering and error timing). The payoff — moving
multi-stage aggregates from ~2.7× toward the ~2.2× single-stage band — is
incremental, not transformative. After the raw-BSON phases (1–6a) brought every
workload to ~1.5–2.7× of mongod, the remaining levers (6b streaming, `find_all`
cursor-batching) are diminishing returns. **Recommend treating the raw-BSON
roadmap as substantially complete** and picking up 6b only if aggregate-heavy
multi-stage pipelines become a stated priority — at which point this workload is
already in the benchmark to measure against.

### Clean full-benchmark baseline (2026-07-20, pinned `73b1790b`/`bedaae02`, unloaded machine)

The write rows above were contaminated; re-measured on a genuinely **quiesced**
machine — a self-gating harness (`scratchpad/wait_and_measure.sh`) that blocks
until the 1-min load average is sustainably < 3.5 before running, then ran
`compare-servers --count 10000 --reps 5` **twice** (both at load ≈ 1.9–2.2, HEAD
unchanged). The two runs agree within noise, so this is the authoritative
post-6a baseline:

| Workload | mongod | SecantusDB-rs | ×mongod (run 1 / run 2) |
|---|---:|---:|:---:|
| insert | ~58–60 ms | ~91–95 ms | **1.6× / 1.6×** |
| find_indexed_range | ~4.4–4.6 ms | ~6.3–6.5 ms | 1.4× / 1.5× |
| find_all | ~7.8–8.1 ms | ~18.7–19.6 ms | 2.3× / 2.5× |
| update_many_half | ~35.5 ms | ~50–52 ms | **1.4× / 1.5×** |
| aggregate_group | ~5.6–5.7 ms | ~12.7–13.0 ms | 2.3× / 2.3× |
| aggregate_multistage | ~5.8–5.9 ms | ~18.1–19.1 ms | 3.1× / 3.2× |
| delete_many_half | ~20–21 ms | ~33–34 ms | **1.6× / 1.6×** |

So on an unloaded machine the **writes sit at ~1.4–1.6× of mongod** (insert 1.6×,
update 1.4–1.5×, delete 1.6×) — near the read/write pack, confirming the earlier
contaminated `0.4–0.5×` were pure load artifacts. The clean mongod baseline also
sharpens the aggregate picture: `aggregate_multistage` is **3.1–3.2×** (vs the
noisy 2.7× estimate) against single-stage `aggregate_group` at **2.3×** — the
extra `$unwind`→`$group`→`$sort` stages cost mongod +0.15 ms but Rust +5.3 ms,
so ~30% of the multi-stage time is the inter-stage materialization 6b would
target. The "6b real-but-modest, deferring recommended" conclusion stands and is
if anything reinforced on the clean ratio.

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

## Finding 4 — the write-path gap to standalone mongod IS the oplog, and its cost is CPU entry-encoding, not the WT write (2026-07-25)

The three-way benchmark spawns a **standalone** `mongod` (no `--replSet`), which
keeps **no oplog**. The Rust server always writes one (change streams /
`local.oplog.rs` / PITR need it). Measured the asymmetry directly by running the
write workloads against a `RustServer(enable_oplog=True)` vs
`RustServer(enable_oplog=False)`, interleaved per rep so machine load cancels
(`scratchpad/oplog_ab.py`):

| workload | oplog ON | oplog OFF | oplog is |
|---|---:|---:|:---:|
| insert | 79.7 ms | 59.2 ms | **26%** |
| update_many_half | 44.6 ms | 23.5 ms | **47%** |
| delete_many_half | 30.7 ms | 20.5 ms | **33%** |

**With the oplog off, the Rust server already matches standalone mongod on writes**
(insert ≈ mongod, update / delete *beat* it) — so the entire write-path gap to the
benchmark's mongod is the oplog. Decomposing the oplog cost into "build + encode the
entry document" vs "WT-insert the entry" (env hook that built+encoded but skipped
the `cursor.insert()`, `scratchpad/oplog_decomp.py`) was the key result: the
**WT insert is essentially free** (≈0, within noise, across all three workloads) —
the entry is already in the batch's open transaction, so appending one more small
row to an in-cache btree page costs nothing measurable. **The whole oplog cost is
CPU: allocating the entry `Document`(s) and BSON-encoding them** — and for an insert
that meant serialising the full document a *second* time (its `o` field) when the
identical bytes had just been encoded for the collection table.

**Fix (rust-oplog-cheap slice, shipped):** assemble each CRUD oplog entry as raw
BSON (`RawDocumentBuf`, mongod field order) and **splice the document body through
un-re-encoded** — an insert's `o` reuses the stored blob, a replacement update's `o`
reuses the already-computed `new_blob`, and only the tiny `o2` / `{$v:2, diff}`
pieces are encoded fresh. `emit_oplog` takes an `OplogEntry::{Doc,Raw}` so the rare
DDL / noop / findAndModify paths keep the owned-`Document` form. Also dropped the two
full-document clones in `compute_update_description` (walk the images directly). No
durability trade-off (the WT write was never the cost) and byte-identical entries
(the whole oplog + change-stream suites pass untouched).

Measured old-vs-new, **mongod interleaved as the load normalizer**
(`scratchpad/write_ab.py`, the two runs' mongod columns agree within ~1%, so the
ratios are directly comparable):

| workload | before ×mongod | after ×mongod |
|---|:---:|:---:|
| insert | 1.17× | **1.02×** (parity) |
| update_many_half | 2.09× | 1.86× |
| delete_many_half | 1.43× | **1.28×** |

Insert reaches standalone-mongod parity; delete closes most of the gap. Update gains
least because its residual oplog cost is the inherent `$v:2` diff walk, not entry
encoding — and that diff is exactly what standalone mongod does not compute at all.
A small read-scan follow-on landed in the same slice (an empty `find({})` skips the
foregone per-doc raw match; the read-only scan reuses each value's allocation instead
of cloning the blob twice) — correct and free, but the full-drain read benchmarks are
at the eager-scan floor, so it is not a measurable mover there.

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

**Expected outcome (as forecast in 2026-07; the slice SHIPPED — see the
2026-07-24 re-baseline at the bottom, which found the residual ceiling is
WiredTiger, not a Rust lock)**: flat 0.5× → ~1.2–1.3× aggregate at 4–8 writers (the
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

## Concurrency re-baseline after RecordId step 1+2 (2026-07-24, pinned `940a25b4`)

The "flat ~0.5× concurrency scaling" line at the top of this doc is **stale** —
that was pre-RecordId. Re-measured on the step-2 tip (`bench.concurrency`,
per-writer collections, 30s/count, batch 100), machine quiesced to load < 2
before the run (the 8-writer figure re-confirmed on a separately-quiesced focused
run; mongod as the control scaled cleanly through 8 in the same run, so the rust
shape is real server contention, not oversubscription):

| writers | Rust docs/s | Rust scaling | mongod docs/s | mongod scaling |
|---|---|---|---|---|
| 1 | 25,750 | 1.00× | 105,930 | 1.00× |
| 4 | **68,195** | **2.65×** (peak) | 368,361 | 3.48× |
| 6 | 48,105 | 1.87× | 459,619 | 4.34× |
| 8 | 33,581 | 1.30× | 494,595 | 4.67× |

(Python, same run, has NEGATIVE scaling — 2,300 → 167 docs/s at 1→8 writers — the
GIL; nothing to do there, it is why the Rust server exists.)

**Two corrections to the record:**

1. **RecordId lifted low/moderate concurrency.** Rust now shows genuinely positive
   scaling to 4 writers (2.65×), not "flat 0.5×". The write-amp reduction (4→3, then
   the step-2 read-hop removal) is why. Update the top-of-doc characterisation.

2. **The remaining cliff is WiredTiger, NOT a splittable Rust lock — the lock-split
   is DONE.** All four steps of "The other axis: concurrency — the design" above
   shipped 2026-07-17 (lock-free reads, per-collection write locks, counters off the
   write path, commit-time conflict handling / the #444/#447 port). Verified against
   the current code: CRUD `insert`/`update`/`delete` take only `coll_lock` (per
   collection — so per-writer-collections hits no shared Rust write lock), and
   `mint_seq_and_ts` holds the oplog mutex only for cheap arithmetic (no I/O). So the
   monotonic 4→6→8 decline (68k→48k→34k) is **WiredTiger-internal** — WAL log
   serialisation / cache eviction / checkpoint pressure in a single-process embedded
   WT — exactly what `docs/concurrency.md` states ("the ceiling is in WiredTiger
   itself"). mongod scales through the same range (4.67×) because it is a
   purpose-built WT host tuned for it; the same WiredTiger, configured differently.
   Per the `rust-write-gap-is-wt-cache` finding, WT cache tuning is only a ~+6–7%
   lever, nowhere near this gap. **There is no cheap Rust-level concurrency win left
   — the ceiling is architectural (embedded WT vs a tuned mongod), and the honest
   guidance for multi-writer-scaling workloads remains "run a real mongod."** Do not
   re-scope the lock-split: it exists. If concurrency is ever revisited, the only
   real lever is WT-host tuning (cache size, eviction threads, dedicated log volume,
   checkpoint cadence), each individually small and each trading memory/durability.
