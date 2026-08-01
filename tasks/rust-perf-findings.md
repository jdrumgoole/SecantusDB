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

## Finding 5 — the concurrency ceiling is specifically the SHARED OPLOG, not generic WiredTiger (2026-07-25)

The 2026-07-24 re-baseline above attributed the 4→8-writer decline to
"WiredTiger-internal (WAL / eviction / checkpoint)" and concluded "no cheap
Rust-level concurrency win left." Measurement refines this materially: the ceiling
is **specifically the shared oplog**, and **WiredTiger itself scales fine**.

Method: the embedded Rust server, 8 external `bench.load_writer` processes, each on
its **own collection** (so data writes go to separate per-collection btrees — no
shared data lock; the oplog shards are the *only* shared write target), oplog ON vs
OFF, interleaved per rep so machine load cancels. The box was heavily / variably
loaded this session (a parallel test suite + macOS spotlight/syspolicy; the oplog-OFF
control swung 8k↔137k docs/s purely by load regime), so numbers are from the
**uncrushed windows** — best-effort, not a quiesced-box measurement:

| 8 writers (uncrushed) | oplog-ON | oplog-OFF | ON/OFF | scaling vs 1-writer (~25.7k) |
|---|---:|---:|:---:|:---:|
| logged oplog (shipped) | ~48.2k | ~124.6k | 2.58× | ON **1.9×** / OFF **4.8×** |
| non-logged oplog tables | ~59.9k | ~135.8k | 2.27× | ON **2.3×** |

**Two conclusions:**

1. **The Rust architecture + WiredTiger scale ~4.8× at 8 writers with the oplog off
   — matching mongod's 4.67×.** The lock-split / per-collection-btree design is not
   the ceiling and neither is generic WT; the *entire* 4→8 concurrency gap to mongod
   is the oplog (which the benchmark's standalone mongod does not maintain). This is
   the concurrency analogue of Finding 4's single-writer result.

2. **The oplog's concurrency cost decomposes, and the WAL log is the *minor* part.**
   Making the oplog + preimage tables non-logged (`log=(enabled=false)`) — WT 7.0
   *does* permit a transaction spanning the logged data table and the non-logged
   oplog table; the full oplog + change-stream suites pass, and only *hard-crash*
   oplog recovery changes (data stays fully logged; clean close checkpoints the
   oplog; WT MVCC keeps committed oplog entries visible to change streams regardless
   of logging) — lifted 8-writer throughput only **~+14–24%** (2.58× → 2.27× penalty;
   an earlier "2.5×" reading was a best-case-single-rep artifact under load, corrected
   by the multi-rep uncrushed runs above). So WAL log volume is a real but modest
   slice. The **majority** of the oplog penalty — oplog-ON retains only ~40% of the
   no-oplog throughput even when non-logged — is the **inherent 2× write / eviction
   pressure**: every CRUD write appends a full oplog entry (an insert's `o` is the
   whole document) to shared oplog btrees, doubling the dirty-page / eviction load a
   standalone mongod never carries. That cost cannot be removed without dropping the
   oplog.

**Levers, by payoff (both unshipped — the box was too load-unstable to validate a
durability-affecting change, and the safe one is modest):**

- **Non-logged oplog tables (modest, safe-ish).** ~+14–24% at 8 writers, as an opt-in
  config (default stays logged/durable). Trade: the oplog becomes checkpoint-durable
  only — a hard crash loses its un-checkpointed tail (change-stream events / PITR
  granularity since the last checkpoint), data unaffected. Reasonable for the
  ephemeral-test audience; a real durability-contract change, so opt-in.
- **Async / decoupled oplog (transformative, risky).** Let writers commit *data only*
  (which scales ~4.8×) and hand pre-built, pre-seq'd oplog entries to a background
  drainer that batch-persists them; `wait_for_oplog` wakes on the drainer's commit.
  Could approach the 4.8× data-only ceiling. Cost: a real rewrite of the emit path
  with ordering, backpressure, change-stream-visibility-latency, and crash-window
  semantics to get right — and it needs a quiesced box to measure. Flagged for a
  future slice, not attempted here.

Harnesses: `scratchpad/conc_oplog_interleaved.py` (interleaved ON/OFF, load-robust),
`scratchpad/conc_on_only.py` (single-arm best-case). The single-writer oplog-encode
win (Finding 4, PR #642) also shaves per-write CPU under the collection lock, a small
independent concurrency helper.

## Finding 6 — WT write-ceiling tuning sweep: modest, quantified (2026-07-25)

Finding 5 pinned the multi-writer oplog ceiling to WiredTiger's aggregate write
throughput (the no-oplog 8-writer rate) and named WT-host tuning as the only
remaining lever. Swept it directly: raw WT connection config appended via a new
`SECANTUS_WT_CONFIG_EXTRA` env hook (WiredTiger takes the last occurrence of a
duplicated key, so an appended clause overrides the default), measuring the no-oplog
ceiling + sync-ON + async-ON at 8 writers on a quiesced box
(`scratchpad/conc_wt_sweep.py`, 3 reps, orphan-guarded):

| WT config | ceiling (no-oplog) | sync-ON | async-ON |
|---|---:|---:|---:|
| baseline (128MB log, evict 4, cache 1G) | 148k | 51k | 69k |
| `cache_size=4G` | 151k (1.02×) | **65k (1.27×)** | 72k |
| `eviction=(threads_min=8,threads_max=8)` | 148k (1.00×) | 51k | 69k |
| `log=(file_max=512MB,prealloc=true)` | **161k (1.08×)** | 54k | 73k |
| all three combined | **162k (1.09×)** | 62k | **79k (1.14×)** |

Conclusions, each modest and each a resource trade — confirming Finding 5's "the
ceiling is architectural, remaining levers are individually small":

- **Log pre-allocation is the biggest write-ceiling lever (~+8%).** `prealloc=false`
  (the default, chosen to keep tiny ephemeral test instances' log footprint small —
  see `wt_config`) makes writers stall on log-file creation under load; `prealloc=true`
  removes that stall. Cost: WT keeps ~2× `file_max` of pre-sized log ready, so at the
  daemon's 2GB `file_max` that's ~4GB of prealloc'd journal — real disk, appropriate
  for a sustained-writer daemon, not for a test instance.
- **A bigger cache barely moves the data-only ceiling (~+2%) but lifts sync writes
  ~+27%** — the read-modify-write path (update/delete reading the doc + index pages)
  is cache-sensitive where a pure-insert ceiling is write-bound. Cost: RAM. Already a
  daemon flag (`--cache-size`, default 1G).
- **Eviction threads: 4 is already enough** (8 = no change).
- **Combined ~+9% ceiling / +14% async / +23% sync.** Real but not transformative.

Shipped: the `SECANTUS_WT_CONFIG_EXTRA` escape hatch (tune WT without recompiling;
default unchanged, an invalid key fails loudly at open). Daemon defaults are left as
they are — the levers each cost disk or memory, so bumping them is a deployment
decision the hatch now makes easy, not a silent default change. Net honest guidance
for multi-writer-scaling workloads is unchanged: WT tuning buys ~10%, and beyond that
"run a real mongod" (or drop the oplog for ~2× via `enable_oplog=False`).

## Finding 7 — allocator (mimalloc) + LTO: the cheap win Finding 1 predicted (2026-07-26)

Finding 1 fingered `malloc`/`realloc`/`free` churn from BSON materialization as a
top on-CPU cost on every read/aggregate path, but no fast allocator or LTO had
ever been tried — the workspace had no `[profile.release]` overrides and the
system allocator throughout. Prototyped both on the embedded `_secantus_server`
extension (the crate the benchmark drives): mimalloc as `#[global_allocator]`
plus `lto = "thin"` + `codegen-units = 1` in `secantus-server-py`'s (and
`secantusdb`'s) release profile.

Four-arm A/B, each `compare-servers --n 10000 --reps 5`, mongod interleaved as
the load normalizer (its columns agreed within ~1% across all four runs, so the
Rust ms deltas are real). Rust ms:

| workload | baseline | LTO-only | alloc-only | combined | combined Δ |
|---|---:|---:|---:|---:|:---:|
| insert | 64.8 | 62.8 | 59.6 | 55.9 | **−14%** |
| update_many_half | 45.7 | 44.8 | 34.8 | 34.1 | **−25%** |
| delete_many_half | 26.9 | 24.7 | 22.0 | 20.9 | **−22%** |
| aggregate_group | 9.8 | 9.4 | 7.5 | 7.4 | **−25%** |
| aggregate_multistage | 15.9 | 14.1 | 12.6 | 11.9 | **−25%** |
| find_indexed_range | 6.7 | 6.7 | 6.5 | 6.5 | flat |
| find_all | 16.0 | 16.5 | 16.2 | 16.3 | flat |

**Attribution: the allocator is the dominant lever** (−18–24% on the alloc-heavy
write/aggregate paths); **LTO is a smaller, roughly-additive contributor** (−2–11%,
biggest on `aggregate_multistage` −11% and `delete` −8%). They stack to the full
~25%. The result maps exactly onto the mechanism — alloc-heavy paths (oplog entry
alloc + BSON encode on writes; the materializing `Vec<Document>` + per-key
`IndexMap` allocations in `$group`/`$unwind`) win big, while the raw-scan reads
(`find` / indexed range), which already splice replies as raw bytes with little
server-side allocation, are flat. Net: single-client insert/update/delete move to
≈mongod parity and `aggregate_multistage` drops 2.7× → 2.0× of mongod — a bigger,
cheaper gain than the deferred streaming-aggregation rewrite (6b) would have
delivered for aggregates, with no logic change or parity re-pin.

**Shipped** (rust-perf-alloc-lto slice): both levers on the extension and the
binary. Trade: `codegen-units = 1` + thin LTO makes the release build slower
(lands on the CI `storage-engine` / wheel jobs) — accepted for the ~25% runtime
win. Behaviour is unchanged (allocator + optimizer only); the functional smoke
(CRUD + `$group` over the rebuilt extension) is byte-correct.

## Finding 8 — PGO on top of LTO: another ~12-19% on write/aggregate paths (2026-07-26)

After Finding 7 (mimalloc + thin LTO) the natural next compiler lever was
profile-guided optimization — untried, and the workspace had no PGO wiring.
Prototyped it on the embedded `_secantus_server` extension: instrument
(`-Cprofile-generate`) → run `compare-servers` to collect a profile → merge
(`llvm-profdata merge --sparse`) → rebuild (`-Cprofile-use`), all on top of the
shipping thin-LTO + mimalloc profile.

Two confirming runs, mongod-normalized (the PGO runs were under equal-or-higher
load, so raw ms understates it). ×mongod, PGO vs the alloc+LTO baseline
(averaged over the two PGO runs):

| workload | alloc+LTO | +PGO | Δ |
|---|---:|---:|:---:|
| insert | 0.98 | 0.82 | **-16%** |
| update_many_half | 0.94 | 0.78 | **-17%** |
| delete_many_half | 0.99 | 0.87 | **-12%** |
| aggregate_group | 1.27 | 1.06 | **-16%** |
| aggregate_multistage | 2.04 | 1.66 | **-19%** |
| find_indexed_range | 1.45 | 1.38 | -4% |
| find_all | 2.08 | 2.08 | flat |

Same shape as Finding 7 — the alloc/dispatch-heavy write and aggregate paths win
(PGO inlines and lays out the hot branches the profile identifies), while the
raw-scan reads, already at the wire floor, don't move. Net: single-client
`aggregate_group` reaches mongod parity (1.06×), `aggregate_multistage` drops
2.0× → 1.66×, and writes beat standalone mongod by 13-22%.

**Shipped** (rust-pgo-split slice) as a 1+2 split, since PGO needs a profile
(not just a flag):
- **Standalone binary** — two-stage per-arch in `release-binaries.yml` (both
  targets build natively, so the instrumented binary runs there to collect an
  on-target profile via `bench/pgo_workload.py`).
- **Wheel extension** — a committed sparse profile
  (`crates/pgo/_secantus_server.profdata.tar.gz`, ~1.6 MB, regenerated by
  `invoke rust-pgo-refresh`); CMake extracts it and feeds `-Cprofile-use` to the
  release build. Function-keyed, so the arm64-generated profile still helps the
  x86_64/musl wheels (`-pgo-warn-missing-function` tolerates the mismatch).

This is the last cheap broad compiler lever. Beyond it the remaining latency
levers are workload-specific engineering (6b streaming aggregation for the
`aggregate_multistage` residual; writev scan→socket for `find_all`), i.e.
diminishing returns — writes now beat mongod and aggregates are at/near parity.

## Finding 9 — async + NON-LOGGED oplog stacks to 2.2× multi-writer; the WAL was the majority of the async residual (2026-07-27)

Finding 5 measured non-logged oplog tables in **sync** mode (+14–24%) and called
the WAL "the minor part"; the async-prototype doc then pinned async's sustainable
ceiling at ~½ the no-oplog rate ("a logical write needs a data write and an oplog
write"). Measuring the **combination** shows both models understated the WAL's
share *in async mode*: with the drainer's oplog writes going to non-logged tables
(`SECANTUS_OPLOG_NONLOGGED=1`, tables created `log=(enabled=false)`), the oplog's
WAL volume leaves the writers' path entirely and the 8-writer rate jumps ~40%
past plain async.

Vehicle: `bench.concurrency --server rust` (standalone `secantusd-rs` daemon,
cache 1G, 8 KiB docs, `insert_many` batch 100, per-writer collections), 15 s
runs, 3 reps interleaved per arm, medians; box quiesced (reps agree within ~2%).
Numbers are daemon-vehicle, so they sit above the embedded-server figures in
Finding 5 / the prototype doc:

| arm (8 writers) | docs/s | ×sync |
|---|---:|:---:|
| sync oplog (default) | 56.3k | 1.00× |
| async (`SECANTUS_OPLOG_ASYNC=1`) | 87.8k | 1.56× |
| async + drainer coalescing | 89.4k | 1.59× |
| **async + coalescing + non-logged oplog** | **125.1k** | **2.22×** |
| sync + non-logged | 58.4k | 1.04× |
| no oplog (ceiling, `SECANTUS_DISABLE_OPLOG=1`) | 191.1k | 3.39× |

Reading: in sync mode non-logging buys ~4% (the writer still pays the shared
oplog btree append inside its transaction — contention, not log volume, binds).
In async mode the btree contention is already gone (background drainer, sharded
btrees), so what remained *was* largely the oplog's WAL bandwidth competing with
the writers' data WAL — remove it and throughput lands at ~65% of the no-oplog
ceiling. Single-writer also gains: ~26k sync → ~49k under the stack (~1.9×), the
inline oplog write leaving the one writer's critical path. Drainer coalescing
(several queued batches per WT transaction, capped 32 batches / 16 MB) is a
small additive win (~1–6%) and is on by default in async mode
(`SECANTUS_OPLOG_ASYNC_COALESCE=0` to disable).

A second quiesced confirmation round reproduced the shape (sync 8w 44.9–45.9k;
async+non-logged 8w 113–117k with coalesce ≈ without + ~1–3%; 1w 26.6k sync vs
49.5k stack) — so the stack's 8-writer gain is **2.2–2.6× of the sync default**
across the two rounds. Two negative results from the same round: WT log
pre-allocation (`log=(file_max=512MB,prealloc=true)`) does **not** add on top of
the stack (its one clean rep landed *below* plain stack, ~100k vs ~115k — with
the oplog non-logged the WAL volume is halved, so Finding 6's +8% prealloc lever
loses its target; not recommended with the stack), and coalescing remains
optional-but-default. Measurement note: two runs in this session were destroyed
by *disk*-side interference at low CPU load (a parallel session's full pytest
suite; an unattributed ~3-min I/O stall) — a load-average gate alone does not
protect 8 KiB-doc write benchmarks; interleave arms and take medians across
reps, discarding collapsed windows (collapsed = ~10× down, unmistakable).

Durability shape of the stack (all opt-in, defaults unchanged): async alone
loses only the un-drained in-memory queue on hard crash; + non-logged also loses
drained-but-un-checkpointed oplog rows (checkpoint-durable oplog). Data tables
stay fully logged in every mode; a clean close flushes the drainer and
checkpoints, preserving the whole oplog. Change streams validated exactly-once
under the full stack (6 concurrent writers × 500 inserts, cluster-wide watch: 0
dups, 0 missing). The daemon also gained `SECANTUS_DISABLE_OPLOG=1` so the
"drop the oplog" lever is reachable without the embedded API.

## Finding 10 — mongod's OWN oplog tax, decomposed: the double-write costs it 26–39%, and its 5.0+ default write concern costs far more (2026-07-28)

Every three-way benchmark in this repo spawns a **standalone** mongod — no
replica set, no oplog — so mongod had never been charged for the thing that
bounds SecantusDB's default write path. Measured it directly:
`bench/mongod_replset_ab.py` runs the concurrency workload (8 KiB docs,
`insert_many` batch 100, per-writer collections) against three mongod
configurations, interleaved, medians of 3 quiesced reps (all reps within ~1%):

| mongod arm | 1 writer | 8 writers |
|---|---:|---:|
| standalone (no oplog) | 113.2k | 503k |
| single-node replset, explicit `w:1, journal:false` | 84.0k | 305k |
| single-node replset, implicit default WC (`majority`) | 11.8k | 68.6k |

Three conclusions:

1. **mongod's pure oplog tax is −26% (1w) / −39% (8w)** — the `w:1` arm pays
   the oplog double-write with no fsync wait, the closest semantic match to our
   sync mode. Same structural cost we pay, at roughly half the rate: our sync
   oplog costs −53% / −71% against our no-oplog ceiling on the same workload.
   mongod's timestamp-slot oplog (concurrent appends into one collection with
   an oplog-visibility point, no shared-append serialisation) is ~2× more
   efficient than our sync path. Our async + non-logged stack retains 65% of
   the ceiling at 8 writers — the same overhead ratio as mongod's 61% — reached
   by relaxing tail durability instead of visibility engineering (Finding 9).

2. **The dramatic cost is the write concern, not the oplog.** Since MongoDB 5.0
   the implicit default is `w:majority`, and on a one-node set a majority ack
   requires a journal fsync: 84k → 11.8k (÷7) at 1 writer, 305k → 68.6k (÷4.4)
   at 8. ~8 ms of fsync latency per acknowledged batch dwarfs the double-write.

3. **Defaults-vs-defaults, SecantusDB now wins this workload.** A single-node
   replset mongod as people actually run it for change streams (implicit
   majority WC) does 11.8k / 68.6k; the Rust server's async + non-logged stack
   does 49.5k / 113–125k — ~4× / ~1.8× faster — at weaker per-write durability
   (we never fsync per ack; mongod's majority ack survives a hard crash).
   At equal semantics (their `w:1` vs our sync) mongod still wins raw ingest
   ~3× (84k vs 26.6k single-writer): its C++ per-byte ingest path, not the
   oplog, is the residual gap.

## Finding 11 — two parked CPU-side write experiments, both negative: neither the per-write mutex nor the oplog re-encode is the bottleneck (recorded 2026-07-29, work 2026-07-23)

Finding 5 pinned the multi-writer ceiling to the shared oplog's WAL / eviction
pressure — a disk/IO wall, not CPU. Two experiments attacked the *CPU* side of the
write path directly to confirm that from the other direction; both were measured on a
quiesced box and **dropped**. Recording them here so the negative results survive the
branches (`rust-oplog-lockfree`, `rust-raw-oplog-splice`) they lived on.

1. **Lock-free `next_nat_seq` — no scaling gain (~−2%).** The per-document oplog
   RecordId counter took a mutex on every insert (100× per `insertMany` batch of 100).
   Replaced it with an `AtomicI64` fetch-add to remove that acquisition from
   `write_nat_entry`. All storage tests passed; single-writer unaffected. A clean
   idle-machine A/B (mutex vs lock-free, 1/2/4/8 writers) measured **~−2%** —
   neutral-to-slightly-negative, **zero** scaling improvement. The mutex was never the
   throughput bound: writers still serialise on WiredTiger's single WAL, so removing
   one of two co-equal walls buys nothing (Amdahl). This is the CPU-side confirmation
   of Finding 5 — lock contention is not the concurrency ceiling.

2. **Raw oplog `o` splice — marginal (~+1%, noise).** The shipped `+12%` raw-BSON
   *insert* write path (PR #608, carries the client's BSON straight to WiredTiger
   instead of decode/re-encoding it up to 5×) has an obvious extension: build each
   oplog entry's `o` field by splicing the stored document bytes rather than
   re-encoding, across all 15 `emit_oplog` call sites. Increment 3 did exactly that and
   was fully correctness-validated (pymongo gauge 99.5% unchanged, change streams
   106/0/100%, `fullDocument` byte-identical). A clean A/B measured the gain at
   **~+1%** — inside the noise floor. The oplog write is WAL/disk-bound, not
   encode-bound (Finding 5 again): cutting the re-encode CPU doesn't move throughput,
   so a 15-site refactor of the shared oplog path (update/delete/change-streams) isn't
   worth it. Only the *insert* raw-BSON path, where the CPU saving is on the hot
   single-writer path rather than the IO-bound oplog append, paid off — and that
   shipped.

A third probe on `rust-oplog-lockfree` — a `SECANTUS_WT_EXTRA` connection-config hook
sweeping WAL `file_max` 128MB→2GB (+13–19% at 4–8 writers) — was **superseded** by the
shipped `SECANTUS_WT_CONFIG_EXTRA` hatch and the log-prealloc lever quantified in
Finding 6; no separate action.

Net: every CPU-side lever tried (remove the mutex, remove the oplog re-encode) confirms
Finding 5 from the opposite side. The remaining real levers are IO/architectural —
non-logged oplog (Finding 9), async oplog drain (Finding 9), write-amplification cuts
via RecordId keying (steps 1–3, #613/#637/#640) — not CPU micro-optimisation of the
write path.

## Finding 12 — Phase-0 truth baseline: ~36% of the sync insert path is OPLOG-PRUNE CHURN, and the "inherent" single-writer gap is 1.9×, not 3.7× (2026-07-30, pinned `efbd32d2`)

The concurrency-parity program's clean-room baseline: quiesced box (load < 2, zero
orphaned shells), pinned SHA `efbd32d2` (post oplog-visibility-point #696), extension
rebuilt from it, 4 arms × 3 interleaved reps × writers 1/2/4/8 (8 KiB docs, batch 100,
15 s, per-writer collections), medians; per-rep spread ≤ ±2% throughout, SHA verified
unchanged before/after. mongod 6.0.16 on the same box.

### The current table (docs/s, medians of 3)

| arm | 1w | 2w | 4w | 8w | scaling@8 (own 1w) | retention@8 |
|---|---:|---:|---:|---:|:---:|:---:|
| Rust sync (default) | 26,447 | 37,936 | 65,809 | 43,682 | 1.65× (peak 2.49× @4) | **23%** of ceiling |
| Rust async+non-logged | 50,460 | 67,195 | 103,387 | 123,073 | 2.44× | **66%** of ceiling |
| Rust oplog OFF (ceiling) | 60,155 | 81,092 | 154,350 | 187,507 | 3.12× | — |
| mongod standalone | 113,031 | 210,510 | 381,893 | 504,486 | 4.46× | — |
| mongod replset `w:1` | 81,026 | — | — | 301,163 | 3.72× | **61%** of standalone |
| mongod replset default (majority) | 12,400 | — | — | 71,463 | — | — |

(Note on normalisation: earlier findings sometimes quoted the OFF arm's scaling
against the *sync* 1-writer rate — that's where "4.8×" came from. Against its own
1-writer rate the ceiling scales 3.12×. Both are honest; be explicit about which.)

Confirmations: async retains 66% ≈ mongod's 61% oplog retention (Findings 9/10 hold);
the replset A/B reproduces Finding 10 within a few percent (110k/493k standalone,
81k/301k w:1, 12.4k/71.5k majority).

### Discovery 1 — the sync write path is ~36% oplog-prune churn under sustained load

`sample`-profiled the embedded server's connection thread under a sustained
single-writer batch-100 load (24.6k docs/s while sampled). The insert subtree
decomposes:

| slice | samples | share |
|---|---:|:---:|
| `emit_oplog_entries` | 2,861 | 46% |
| — of which `prune_oplog_inner` + `read_oplog_shards_tagged` + `peek_entry_ts` | **2,240** | **36% of the whole insert path** |
| `__wt_txn_commit` (≈ all `__wt_log_write`, the WAL) | 2,280 | 37% |
| `__wt_btcur_insert` (the actual row inserts) | 901 | 14% |
| `write_nat_entry` / serde / framing / misc | ~350 | ~6% |

Mechanism: the bench writes 24.6k oplog rows/s; `oplog_max_entries` defaults to 100k,
reached ~4 s into any sustained run. From then on the opportunistic prune (every 1000
emits) must delete ~as many rows as arrive to hold the cap — **every insert pays ~1
oplog-row delete plus a share of the k-way merge scan that finds the doomed rows.**
The earlier "prune is O(deleted) not O(oplog)" fix made the sweep proportional to the
delete count, but at steady state the delete count ≈ the insert rate, so the churn is
structural. mongod never pays this shape: its oplog is a capped collection whose
truncation is a cheap wholesale range drop, not per-row cursor deletes found by a
17-table merge. Levers (for the emit-path-hygiene PR / the sweep): move the prune off
the write path (timer/background), delete by seq-range per shard without the merge
scan, WT range-truncate, and/or raise the default cap for daemon deployments. Upper
bound if fully removed: ~1.5× single-writer sync (26.4k → ~41k) and a bigger share at
4–8 writers where every writer prunes.

Background-thread note: WT eviction/reconciliation threads spend heavily in
`__rec_write`/`zlib_compress` (block compression of data pages) — off the insert
thread's critical path but part of the aggregate write ceiling; the sweep's
compressor arm should test data tables too, not just the oplog.

### Discovery 2 — the "inherent" single-writer gap is ~1.9×, not 3.7×

At TRUE equal semantics — both servers with no oplog — the single-writer gap is
**113,031 / 60,155 = 1.88×**, far below the forward-plan's "even WAL-free we're 3.7×
off" (measured before raw-BSON #608, RecordId #613-640, mimalloc/LTO #660, PGO). At
equal *oplog* semantics (their `w:1` vs our sync) the gap is 81,026 / 26,447 = 3.06× —
but ~36% of our side is the prune churn above, so the structural per-op gap at that
comparison is closer to ~2×. Consequence for the program: Phase B (per-op efficiency)
starts from a much better base than planned; killing the prune churn plus the
log-only-the-oplog structural probe (arm D) plausibly covers most of the remaining
distance to the mongod ratio without touching the "inherent" C++-vs-Rust story at all.

Harness: `scratchpad/phase0_matrix.sh` (this session), `bench/mongod_replset_ab.py
--reps 3`, `sample <pid> 10 1` on the server process under `bench.load_writer
--batch-size 100`.

## Finding 13 — the oplog append-path sweep: winners stack to 54% retention fully durable (102.8k @8w), and the mongod-architecture probe adds only the last +11% (2026-07-30, post-#700)

The PR-3 sweep (`bench/oplog_sweep.py`, hooks `SECANTUS_OPLOG_SHARDS` /
`SECANTUS_OPLOG_TABLE_EXTRA` / `SECANTUS_DATA_NONLOGGED`; 12 configs × 2 interleaved
reps, 12 s, 1/8 writers, 8 KiB docs, batch 100, quiesced box, post-#700 code).
Retention = median vs the same-session no-oplog ceiling (59.6k 1w / 172.9k 8w).

| config | 1w | 8w |
|---|---:|---:|
| sync default (16 shards) | 32.4k (54%) | 74.8k (43%) |
| shards 1 / 2 / 4 / 8 | ~33–34k (56%) | 83.7 / 86.3 / 80.6 / 86.7k (47–50%) |
| oplog `block_compressor=none` | 31.1k (52%) | **32.3k (19%) — craters** |
| oplog `memory_page_max=10MB` | 30.3k (51%) | 79.9k (46%) |
| oplog `split_pct=100,leaf_page_max=128KB` | 32.2k (54%) | 88.8k (51%) |
| conn `log=(file_max=512MB,prealloc=true)` | 30.8k (52%) | 68.7k (40%) |
| conn `cache_size=4G` | 35.8k (60%) | 93.9k (54%) |
| `SECANTUS_DATA_NONLOGGED=1` (mongod split, measure-only) | **39.6k (66%)** | 79.5k (46%) |

Combo runs (8w, 2 reps, same session; ceiling 190.3k):

| stack | 8w | retention |
|---|---:|:---:|
| 2 shards + append-split + cache 4G — **fully durable** | **102.8k** | **54%** |
| + data-nonlogged (crash-unsafe; the Phase A′ shape) | **114.1k** | **60% = mongod's 61% ratio** |

Conclusions:

1. **The 16-way oplog sharding is now pure overhead.** Post-RecordId (#613-640) and
   post-prune-fix (#700), every lower shard count beats 16 at 8 writers (+12-16%),
   and 1 ≈ 2 ≈ 8. The rightmost-page append contention the sharding was built
   against (0.60×→2.47× at the time) no longer binds; what's left is merge/cache
   overhead proportional to table count. Simplifying toward mongod's single-table
   shape is a win, not a risk. (Kept as a routing default question for PR 4 —
   routing-only, the read side scans all tables regardless.)
2. **Never turn oplog compression off** — 8w throughput craters to 19% retention.
   Bigger uncompressed pages mean more eviction/IO volume; zlib is load-bearing
   under write pressure, the exact reverse of the single-writer CPU intuition
   (and consistent with log-compression backfiring in the forward plan: CPU is
   not the constraint, IO volume is).
3. **Cache is the strongest single knob** (+26% at 8w) — eviction pressure again,
   matching Finding 6's sync-arm hint. `log prealloc` HURTS at 8w post-#700
   (−8%), reversing Finding 6; the prune fix changed the IO profile enough that
   pre-sized log files no longer pay.
4. **The mongod-architecture hypothesis (Finding 12's arm D) is materially
   weakened at 8 writers.** Data-tables-unlogged alone buys +6% at 8w — after
   the prune fix, the WAL is no longer the binding constraint (Finding 5's
   dirty-page/eviction majority reasserts). Where it shines is single-writer
   (+22%, the best 1w config measured). On top of the config winners it adds
   102.8k → 114.1k (+11%), reaching the mongod retention ratio exactly — so
   Phase A′ (replay-on-open recovery to make it crash-safe) is the *last* step
   to the ratio, not the main one, and its cost/benefit is now +11% at 8w /
   +22% at 1w for a substantial recovery build. Decision: park Phase A′ until
   the config winners have shipped (PR 4) and re-measure; the cheap 80% of the
   distance is config.

Net journey this session (sync durable, 8 writers): 41.5k (pre-#700) → 74.8k
(#700) → 102.8k (config winners, fully durable) — 2.5× — with the ceiling at
190k and mongod's replset-w:1 at ~301k on the same box.

## Finding 14 — Phase A′ landed: replay-on-open is correct, but durability ANCHORING is the real cost of the mongod split (2026-07-31)

Phase A′ (log-only-the-oplog + replay-on-open recovery) shipped as opt-in:
stable-checkpoint marker in the always-logged oplog-meta table, a periodic
checkpoint thread (60s default, `SECANTUS_CHECKPOINT_SECONDS`), an
on-demand anchor when the prune clamp blocks a genuine cap excess, and
idempotent oplog replay at open — proven by a hard-kill harness (SIGKILL
mid-load; every `sync_on_commit` acknowledged write recovered, including
full replay from genesis with no checkpoint ever taken).

The measured surprise: **Finding 13's arm-D numbers (+11% @8w) were the
UNANCHORED mode.** With anchoring live at any practical cadence, a
sustained 8-writer cap-pressure load pays the periodic checkpoint of a
hot, fully-dirty unlogged working set:

| anchored cadence (8w, 15s, cap-pressure) | docs/s |
|---|---:|
| never (probe shape; unbounded oplog) | 122.4k |
| 2s | 57.1k |
| 10s | 45.7k |
| 60s + demand-anchor (shipped default) | ~38–46k |
| logged default (no A′), same build | ~92k |

Single-writer: ~+5% (36.8k vs 34.9k). Decomposition: between anchors the
unlogged tables accumulate the ENTIRE working set as dirty pages (nothing
was WAL-reconciled), so each checkpoint writes it wholesale while writers
stall; the prune clamp additionally holds every post-stable oplog entry,
so cap-pressure loads bloat until an anchor lands (the demand-anchor
bounds this but forces the expensive checkpoint sooner). mongod amortises
the same cost with incremental checkpointing + eviction tuned for exactly
this shape. **Decision: the default flip (A′-2) is parked** — the mode is
correct, recoverable, and opt-in for read-heavy / single-writer / bounded
workloads; taming the anchored-checkpoint cost (incremental checkpoints,
eviction_dirty_target tuning for unlogged tables) is the priced next
investigation if the flip is ever to happen.

## Finding 15 — the anchored A′ equilibrium is CAP-DRIVEN, and dirty-eviction tuning makes it worse (2026-07-31)

Swept the named levers against the anchored-checkpoint cost (Finding 14) via
the existing hooks (`SECANTUS_DATA_NONLOGGED=1`, live anchoring, 8 writers,
30 s, 2 reps):

| arm | 8w docs/s |
|---|---:|
| control (10s cadence) | ~49.9k |
| `eviction_dirty_target=2,trigger=5` | ~30.2k (−40%) |
| `eviction_dirty_target=1,trigger=3` | ~5.9k (−88%) |
| dirty 2/5 + 8 eviction threads | ~32.9k |
| dirty 2/5, 30s cadence | ~32.0k |
| cadence 3600 (demand-anchor only) | ~50.7k |

Two conclusions:

1. **Aggressive dirty writeback is a dead end** — an append-hot unlogged
   working set gets written repeatedly as pages refill: pure write
   amplification, catastrophically so at target 1%.
2. **Every sustained arm converges at ~50k because the demand-anchor makes
   the equilibrium OPLOG-CAP-DRIVEN, not cadence-driven**: at cap pressure
   (100k entries ÷ ~50k docs/s) the cap-blocked prune demands an anchor
   every ~2 s regardless of the configured cadence. (Finding 14's 122k
   "unanchored" figure predates the demand-anchor; with it, the unbounded
   arm no longer exists — correctly.) The one cheap untested lever, for
   whenever the flip is revisited: a larger `oplog_max_entries` in this
   mode proportionally reduces anchor frequency (cap 1M ≈ one anchor per
   ~20 s at this rate). The default flip stays parked.

## Finding 16 — Phase B experiment 1: the per-collection-recno layout hypothesis is FALSIFIED (2026-07-31)

`bench/wt_poc/wt_layout_bench.c` A/Bs the engine's table layout against
mongod's at the raw-WT level (pure C + pthreads, ~1 KiB rows, ascending
keys, compression off in all arms to isolate layout; 60k rows/thread,
2 reps, means):

| layout | 1t | 4t | 8t |
|---|---:|---:|---:|
| `q` — per-thread table, bare int64 key (mongod shape) | 532k | 879k | 824k |
| `ssq` — per-thread table, (db, coll, id) composite key | 494k | 858k | 834k |
| `ssq-shared` — 2 threads/table, composite key (shard collision) | 503k | 897k | **1017k** |

The (S,S,q) key shape costs **~7% at one thread and ~nothing at 4–8**, and
btree sharing is not a penalty at this scale — the shared arm WINS at 8
threads (fewer trees → better cache/eviction locality). Against the
plan's ≥1.5× decision gate this is a falsification by an order of margin:
**the per-collection-recno storage-format epoch is not justified and is
closed.** The residual ~1.9× single-writer gap to mongod lives above raw
WiredTiger — per-operation work in dispatch/BSON/session-txn handling and
mongod's ingest pipeline — exactly the "Lever B: do NOT pursue for the
reward / Lever C: research-scale" territory the forward plan already
priced. With Phase 0/A/A′ shipped and B's gate concluded NO-GO, the
concurrency-parity plan's experimental program is **complete**: every
phase has either shipped or been closed with data.

## Finding 17 — post-Phase-C confirmation matrix; the async prune's measured cost (2026-07-31)

A full pinned 3-rep interleaved 4-arm matrix on `01d45bea` (post-#716/#717,
PGO-staged daemon, standard 30s / batch-100 / per-writer-collections
methodology) re-confirms the durable-path improvement, with mongod as the
session normalizer landing within 3% of the published table:

| arm (medians, docs/s) | 1w | 2w | 4w | 8w | vs published |
|---|---:|---:|---:|---:|---|
| Python | 11.7k | 8.6k | 9.9k | 7.6k | ±3% |
| Rust sync (default) | 34.0k | 47.3k | 77.5k | 89.4k | −3..−12% (2w rep-1 outlier; scaling 2.63×, monotonic) |
| Rust async+nonlogged | 45.2k | 57.3k | 88.1k | 104.7k | −10..−12% |
| mongod (standalone) | 105.3k | 202.2k | 358.2k | 468.2k | −3% |

The async column's larger deviation decomposes cleanly. A cargo-vs-cargo A/B
(neither binary PGO'd; 3 interleaved reps, async 8w) of pre-#716 (`75d122a2`)
vs current: **121.3k vs 113.0k median — the #716 opportunistic prune costs
~6% at 8 writers**; the remainder matches the session's ~3–5% box headwind
seen equally on the mongod and sync arms. This cost is deliberate: before
#716 an async store never pruned from write volume at all, so every published
async number was measured on a store whose oplog grew without bound for the
duration of the run — an unshippable configuration. The sync path has always
paid the (post-#700 key-only) prune; #716 makes async pay the same, honest
price. The published async column (50.4k/66k/100.3k/119.2k, 0.6.0b5) should
be read as ~5% optimistic until the next release re-baseline.

## Finding 18 — Tier-1 micro-opt measurements: the compare fast path is large, the catalog cache is a measured zero (2026-08-01)

From the codebase micro-optimisation review (gate: ship at >=1% on the
target bench, drop below it):

- **`numeric::classify` fast path (#730, SHIPPED)**: allocation-free
  int/double comparisons. COLLSCAN int-range filter drain **+10.8%**
  (1.98M → 2.19M docs/s), int-bound-vs-double-field drain **+48.7%**
  (2.03M → 3.03M), in-engine sort neutral. Interleaved A/B, release
  builds, non-overlapping distributions. The historic "BSON alloc churn"
  hot spot had a second layer below mimalloc: the digit-form NumVal built
  a String + Vec per operand per comparison.
- **`CollMeta` one-decode options view (SHIPPED)**: insert/replace/delete
  decoded the same collection-options row 2-3× per op. Paired A/B on
  two-index batch inserts: **+2.3%, 5/5 positive pairs** (plain-insert arm
  within noise). Strictly-less-work change, no added machinery.
- **Per-(db,coll) index-catalog snapshot cache (DROPPED — measured ~0)**:
  a seqlock-generation cache of decoded `IndexDesc`s, correct under
  concurrent DDL (fresh-session fills, user-txn bypass, commit bumps) and
  fully green on the storage suites — but two quiet-box 5-rep A/Bs put the
  two-index insert delta at **+0.4% / +2.3%-with-outlier ≈ noise**, and
  the run-to-run drift (~3%) exceeded the effect. Mechanism: the per-thread
  WT cursor cache already makes the K≤2 catalog walk cheap (~a search +
  two tiny decodes), and the cache's own per-op cost (two String key
  allocs + mutex + Arc clones) cancels most of the saving. Might pay at
  K≥5 indexes, but that isn't the representative workload; complexity not
  shipped for noise. (Branch existed as `microopt-catalog`, deleted; the
  diff is reconstructible from this finding + #730-era review notes.)

Measurement note for future micro-opts on this box: with a parallel
session active the practical noise floor is ±2-3% even at load<4 —
sub-2% effects need paired designs (per-pair deltas, sign tests), not
mean comparison.
