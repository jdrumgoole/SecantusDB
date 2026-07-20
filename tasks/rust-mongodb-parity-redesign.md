# Plan: redesign the Rust server to approach mongod performance

Status: **active** (2026-07-20). Goal set by Joe: "redesign the rust server so it
approaches MongoDB performance, ignore previous simplicity goals." The global-lock
/ "don't think about WT's MVCC" simplicity constraint is **lifted**.

Branch/worktree: `../SecantusDB-mongo-parity` (`mongo-parity-redesign`), pinned
measurements only. Each phase lands as its own measured PR.

## Why this is viable where the Python effort wasn't

`tasks/wt-concurrency-plan.md` ran this for the **Python server** and died on a
hard wall (Phase 2.4–2.6): the Python hot path (WT SWIG `packing.py`, bson decode)
is pure Python, and the **GIL** serializes it across cores — no lock work fixes
that. **The Rust server has no GIL** — its accept loop runs on GIL-released native
threads. So the reason the Python concurrency effort was parked does not bind here.
That is the whole reason redesigning the *Rust* server can reach where the Python
server structurally cannot.

## Diagnosis (measured + mapped, 2026-07-20)

Two dimensions; the raw-BSON roadmap (phases 1–6a) already addressed single-client
**latency** (2.1×–4.5× → 1.5×–2.5× of mongod). The open gap is **concurrency**:
mongod scales write throughput ~4.1× at 8 writers; the Rust server is flat/≤1×.

**Serialization points (Explore map of `crates/secantus-storage/src/lib.rs`), ranked:**

1. **Per-collection write lock held across the whole WT transaction**
   (`coll_lock` at `insert_one:2708`, `insert:2783`, `update_matching:5577`,
   `delete_matching:5780`, released only when `retry_write_conflicts` returns).
   Two writers to the **same** collection fully serialize — *discarding WT's MVCC,
   which exists precisely to handle this*. This is the ~0.35× shared-collection number.
2. **Global `oplog` mutex, taken by every write** (`mint_seq_and_ts:1816`, notify
   block `:1872`) + the **single shared oplog btree tail** (`OPLOG_TABLE`, one
   `q`-keyed table). Oplog on by default → every write on every collection
   rendezvous here. This is the primary thing serializing writers to **different**
   collections (~0.38×).
3. **No admission control** (confirmed absent — unbounded thread-per-connection,
   `server/src/lib.rs:412`). Under load, contention degrades unbounded instead of
   being shed → throughput drops *below* 1× at high writer counts (convoy).
4. **No group commit** in the Rust layer (`with_statement_txn:1772` commits
   per-write; only WT's WAL coalesces).
5. **All collections share single WT tables** (`secantus_documents` key `SSu`,
   `secantus_index_entries` key `SSSu`) — mongod uses **one WT table per
   collection**. So even "distinct collection" writers hit the same btree.
6. **WT connection config not tuned for write concurrency**
   (`wt_config`, `storage/src/lib.rs:407`): `cache_size=256M`, no `eviction=(threads…)`,
   `log=(enabled,file_max=10MB)`. mongod runs multiple eviction threads, a larger
   cache, and tuned logging.

**The ceiling clue that reframes everything:** the pure-C `wt_poc` harness
(`bench/wt_poc/wt_pthread_bench.c`) uses **one table per thread** (`table:t%d`),
no GIL, no shared table — the ideal case — and still caps at **~1.3×** on this
Mac. mongod gets **4.1× on the same box**. So a ~3× gap is *WT usage/config*, not
hardware and not (only) our locks. Removing locks is necessary but not sufficient;
we must also drive WT the way mongod does (eviction threads, log/group-commit
posture, per-collection tables).

## Correctness gates (every phase — non-negotiable)

- `tests/test_mongo_server_concurrency.py` (`--server rust`) — the integrity suite:
  exact-count parallel inserts, no-lost-update `$inc`, unique-index single-winner
  (losers see 11000), findAndModify exclusivity/completeness, MVCC readers never
  lose a stable doc, distinct-collection parallelism, change-stream sees every
  concurrent insert. A rearchitecture that breaks any of these is wrong, full stop.
- `./inv rust-gate` (clean workspace + WT crates + parity + full pytest).
- Driver gauges (`validate --server rust`, sub-agent) non-regressing.
- `./inv rust-parity` byte/bool parity of the pure engines.
- **Measure** `bench.concurrency --server rust --writers 1,2,4,8` (separate +
  `--shared-collection`) and `compare-servers`, on an **unloaded** machine (gate on
  1-min load < 3.5 — see `scratchpad/wait_and_baseline.sh`), pinned SHA, before/after.

## Phases (cheapest / highest-information first)

### Phase 0 — Honest baseline *(done, 2026-07-20, pinned `95ee66b9`, unloaded)*

`bench.concurrency --duration 20 --writers 1,2,4,8`, docs/s and scaling-vs-1-writer:

| writers | Rust separate | Rust shared | mongod separate |
|---|---|---|---|
| 1 | 10,250/s (1.00×) | 11,695/s (1.00×) | **110,328/s (1.00×)** |
| 2 | 0.54× | 0.65× | 1.85× |
| 4 | 0.39× | 0.67× | 3.40× |
| 8 | 0.38× | 0.66× | **4.55×** |

**The dominant finding is single-writer, not scaling:** mongod does 110k docs/s
with ONE writer; we do 10k — a **~10.7× per-command gap** in the sustained
small-batch (100-doc `insert_many`) regime that `compare-servers` (one 10k-batch,
1.6×) entirely hid. Each 100-doc batch takes ~8ms for us vs ~0.9ms for mongod.
Leading suspect: **write amplification** — per inserted doc we write the doc row +
index entries + *two* natural-order rows (`secantus_natural` + `_seq`) + a
BSON-encoded oplog row (~5 WT writes/doc) where mongod writes ~2. Needs a profile
to confirm before acting.

### Phase 1 — WT config tuning *(done, measured)*
Added `eviction=(threads_min=4,threads_max=4)` + `log(file_max=128MB)`. Result:
single-writer +22% separate / +10% shared (205k→251k / 234k→257k), shared 8-writer
+19% (153k→183k). **Scaling curve unchanged** (0.33–0.71×). Config was a minor
lever; kept (real, low-risk). The ~8.8× single-writer gap that remains is
structural, not config.

### Phase 1 — WT connection-config tuning *(cheap, do first — highest info/risk ratio)*
Add `eviction=(threads_min=4,threads_max=4)`, raise `cache_size`, tune
`log`/`checkpoint` toward mongod's write posture in `wt_config`
(`storage/src/lib.rs:407`). **Re-measure `wt_poc` AND `bench.concurrency`.**
Hypothesis: moves the ceiling with near-zero code risk. If the pure-C harness lifts
off 1.3×, the whole effort is far more promising; if it doesn't, the ceiling is
deeper (log architecture) and Phase 5 rises in priority. Either way it's a
one-string change that tells us where the wall really is.

### Phase 1.5 — Write-path de-amplification *(the single-writer lever — confirmed by code-read)*

The ~8.8× single-writer gap is write amplification, confirmed by reading
`insert` (`storage/src/lib.rs:2776-2907`) with 8KB docs (`bench/load_writer.py:56`).
**Per inserted doc, oplog on, we do ~2–3 full BSON round-trips of the ~8KB doc and
~4 WT writes; mongod does ~1 encode-free write pass and ~2 WT writes.** The levers,
each measurable in isolation via `bench.concurrency` single-writer:

1. **Skip the decode→encode round-trip when `_id` is present** (`:2804` `decode_doc`
   then `:2829` `encode_doc`). Peek `_id` from the raw bytes (Phase-2 `matches_raw`
   machinery) and insert the *original* blob unchanged — no re-serialize. Saves
   ~2 × 8KB BSON ops/doc. Directly analogous to the raw-BSON read work, applied to
   writes.
2. **Reuse the doc blob for the oplog entry `o`** (`:2878` moves the decoded `Document`
   into the entry, then `emit_oplog` re-encodes it). Carry the already-encoded blob
   into the oplog row instead of re-encoding. Saves ~1 × 8KB encode/doc.
3. **The natural-order index — 2 extra WT writes/doc** (`write_nat_entry`, `:2867`:
   `secantus_natural` + `secantus_natural_seq`). mongod's RecordId is intrinsic.
   Options: a single combined nat entry, WT record-number column, or lazy/derived
   ordering. Biggest WT-write reduction; needs `$natural` / insertion-order
   correctness preserved. Structural — sequence after 1+2.
4. **Drop the `unique_conflict` pre-check** (`:2815`, one lookup/doc/unique-index) →
   rely on `cursor.insert(overwrite=false)` `WT_DUPLICATE_KEY` (already the backstop
   at `:2842`). Also the Phase-2 correctness prerequisite.

This is the highest-value latency workstream — the single-writer base is what all
the scaling multiplies. Gate: full CRUD + concurrency integrity suites + parity.

**Measured negative result (2026-07-20):** lever 1 (skip re-encode when `_id`
present) gave **~0 improvement** (251k → 256k single-writer, within noise). So BSON
*re-encode* is NOT the bottleneck. Reverted.

**Profile verdict (`sample` on debug-symbol binary, single-writer load):** time is
spread across WT's write machinery — `__wt_evict_thread_run`, `__wt_reconcile`,
`__wt_block_write`, `__wt_page_in/out/swap`, cursor-cache churn — essentially **no
`secantus_*` frames**. The bottleneck is **WT itself under cache pressure + write
amplification**, not our Rust code, not locks, not BSON. Two levers fall out:

- **Cache size — a MEASUREMENT-ARTIFACT, not a win (corrected).** A short cache A/B
  (`load_writer --count 200000`, ~9s) showed 1G 12.1k/s → 8G 21.3k/s and I nearly
  shipped a "+75%" auto-cache change. The honest **20s steady-state** concurrency
  bench debunked it: 8G gives **239k/251k single-writer — flat vs 1G's 251k/257k**.
  The short 8G run simply finished *before* WT's dirty-eviction trigger engaged
  (transient), while the 1G run was already at steady state. **Steady-state write
  throughput is disk-write-*volume* bound, not cache bound** — bigger cache doesn't
  help. Change reverted. Lesson: always measure steady-state (20s), never a
  short-count run, for write throughput.
- **Compression — the real steady-state suspect (mongod-parity).** The bench doc is
  `"x"*8192` — pathologically compressible — and **mongod block-compresses (snappy)
  by default while our WT tables don't** (`DEFAULT_CONFIG` / table creates set no
  `block_compressor`). So mongod writes ~nothing to disk per doc; we write the full
  8KB. Since steady state is disk-volume bound, compression could be a large,
  legitimate lever (and matches mongod). *Next experiment — verify snappy is in the
  WT build, enable `block_compressor` on the data/index/oplog tables, measure 20s
  steady-state.*
- **WT-op reduction:** independently, we do ~4 WT writes/doc (doc + 2 natural-order +
  oplog) vs mongod's ~2. The natural-order index (`write_nat_entry`) is the target —
  fewer writes = less disk volume. Sequence after the compression result.

**Compression build-integration — WIP, blocked at runtime registration (2026-07-20).**
Wired zlib block-compression into the self-contained WT build (uncommitted on
`mongo-parity-write-deamp`). Five sequential hurdles, four resolved:
1. `ENABLE_SNAPPY` **finds** a system snappy (none in the wheel build) → switched to
   zlib (`libz` is ubiquitous). *(snappy is a follow-up: bundle its source.)*
2. `ENABLE_ZLIB` + `HAVE_BUILTIN_EXTENSION_ZLIB` are mutually exclusive → keep only
   the builtin flag (`CMakeLists.txt`).
3. Builtin `zlib_compress.c.o` (now in `libwiredtiger`) references `libz`'s
   inflate/deflate → link `-lz` (`crates/secantus-wt/build.rs`, linux+macos).
4. `block_compressor=zlib` on the doc/oplog/preimage tables
   (`storage/src/lib.rs` BOOTSTRAP).
5. **RESOLVED:** runtime `unknown compressor 'zlib'` was a **stale-lib link**, two
   layers: (a) `secantus-wt/build.rs` and `_rust_env()` both prefer
   `/tmp/wt-build` (a 3-day-old dev-sandbox WT with **no** zlib) over this
   checkout's fresh `build/*/wt-build`; (b) passing `SECANTUS_WT_LIB` as a
   *relative* path broke bindgen. Fix: build with **absolute**
   `SECANTUS_WT_LIB`/`SECANTUS_WT_INCLUDE` pointing at the fresh `build/*/wt-build`
   (the fresh lib's `conn_api.o` correctly references `zlib_extension_init`).
   *(Merge concern: the CI/wheel build produces the fresh lib in-tree, so this is a
   local dev-sandbox shadow only — but worth a `build.rs` note that a stale
   `/tmp/wt-build` shadows a config change.)*

**Compression WORKS — and it's the biggest lever, on BOTH axes.** Smoke: 5000×8KB
`"x"*8192` docs → `secantus_documents.wt` **1.49 MB vs ~40 MB raw ≈ 27×**. Measured
(20s steady-state, unloaded):

| | Phase 1 | zlib compression | Δ |
|---|---|---|---|
| single-writer (sep) | 12.5k/s | **15.6k/s** | +25% |
| scaling 2w / 4w / 8w (sep) | 0.62 / 0.41 / 0.33× | **0.95 / 0.70 / 0.62×** | ~2× at 8w |
| shared 8w | 0.75× | 0.90× | +20% |

**Confirms the profile:** the bottleneck is disk-write *volume* — cutting it lifts
single-writer AND roughly doubles multi-writer scaling (less data → less disk/WT
contention for parallel writers). Caveat: the bench doc is 27×-compressible; real
data ~2–4×, so the real-world win is proportionally smaller but real on both axes.

**To land (cross-platform build-integration concerns):**
- The wheel build must produce a zlib-enabled WT on every target. `libz` links from
  the macOS SDK and manylinux/musl (zlib present); **Windows** has no default `libz`
  — the `build.rs` `-lz` is gated to linux+macos, so the Windows wheel would build
  WT with `HAVE_BUILTIN_EXTENSION_ZLIB` but fail to link. Either gate the CMake flag
  to non-Windows, or bundle zlib. Decide before merge.
- On-disk format change: compressed tables must round-trip reopen/backup/PITR — WT
  handles decompression transparently, but the durable-lane + reopen tests must be
  green (the gate + CI durable lane cover this).
- snappy (mongod's default, better CPU/ratio balance) is the follow-up once the
  compressor build path is proven; needs its source bundled (WT only *finds* snappy).

### Phase 2 — Drop the per-collection CRUD lock → WT-MVCC-native *(biggest same-collection lever)*

**Feasibility analysis (2026-07-20, de-risked):** the `coll_lock` does NOT protect
the seq/ts/`next_nat_seq` counters — those mint under the dedicated `oplog` mutex
(`lib.rs:1959`; the seq can't be a bare atomic because `wait_for_oplog` pairs it
with `oplog_cv`). So `coll_lock` only serializes same-collection writers *across
their WT transaction* — which WT's MVCC + the existing `WriteConflict`/
`WT_DUPLICATE_KEY` retry (`retry_write_conflicts`) already handle. Carve-outs:
capped-collection trim needs a **narrow** per-collection lock (avoid double
eviction); `maybe_mark_multikey` is idempotent (benign race); unique-index races →
`WT_DUPLICATE_KEY` (already the backstop). So the change is: drop `coll_lock` from
the CRUD write path, add a narrow trim-only lock. **Correctness-critical** — this
removes write serialization from a database, so it must land with the full
`test_mongo_server_concurrency` integrity suite green *plus* extended stress
(exactly-one-winner, no-lost-update, findAndModify exclusivity) and deserves
focused implementation + review, not a rushed change. The gate exists; the analysis
is done; the implementation is the next focused session's work.

Stop holding `coll_lock` across the WT transaction on the CRUD write path; let
concurrent same-collection writers run concurrent WT transactions and rely on WT
conflict detection + the **existing** `WriteConflict` retry. Carve-outs that still
need per-collection serialization get a *narrow* mechanism, not the broad lock:
- **Unique index**: already backstopped — `cursor.insert(overwrite=false)` →
  `WT_DUPLICATE_KEY`; drop any pre-check, translate the WT error.
- **Capped-collection FIFO trim**: needs per-collection exclusivity → a dedicated
  short-held trim lock, not the whole-write lock.
- **Natural-order seq counter / multikey flag**: make the counter atomic; multikey
  flag set is idempotent (benign race).
Gate hard on the concurrency integrity suite (it already covers every hazard).

### Phase 3 — De-serialize the oplog *(cross-collection lever)*
Replace the global `oplog` mutex seq/ts mint with an **atomic** reservation
(lock-free `fetch_add` of the seq; ts from a small lock-free scheme or a striped
mint). The reserve-optime-then-write seam already exists (`:1815` vs `:1842`).
Assess the single oplog btree tail: if it's the residual hotspot, consider
per-thread/per-collection oplog staging buffers flushed in order, or WT log-slot
tuning. Keep change-stream ordering + `postBatchResumeToken` correct
(`test_change_stream_sees_every_concurrent_insert`).

### Phase 4 — Admission control *(stops the sub-1× convoy collapse)*
A read/write **ticket** semaphore (mongod's model — bounded concurrent storage
ops) so high writer counts queue instead of thrashing. Bound ≈ cores. This is why
8 writers currently do *worse* than 2.

### Phase 5 — One WT table per collection *(deep; on-disk format change)*
If cross-collection scaling stays capped by the shared `secantus_documents` /
`secantus_index_entries` btrees, move to mongod's model: a WT table per collection
(and per index). Biggest change — on-disk format + migration/reopen path, the
`(db,coll)`→table-name registry, all cursor routing. Gated on Phase 1/3 showing the
shared btree is the actual wall. Storage reopen/PITR/backup tests must stay green.

### Phase 6 — Group commit + latency finishers
Application-level commit coalescing across concurrent writers (matters most on the
`sync_on_commit=true` durable path). Then the latency levers deferred earlier:
SBE-style slot aggregation (the 6b streaming work) and covered queries (needs an
index-entry format that preserves the original value — today's sort-keys are lossy).

## Sequencing rationale
1 (config) and 0 (baseline) are cheap and tell us where the wall is. 2 (same-coll
lock) is the biggest contained code lever. 3 (oplog) unblocks cross-collection. 4
(admission) stops the collapse. 5 (per-collection tables) is the deep architectural
change, funded only if 1/3 prove the shared btree is the ceiling. 6 finishes.
Re-order on measured numbers after each phase — the wt_poc 1.3× result means we must
verify each lever actually moves the needle before funding the next.

## Post-compression re-profile (2026-07-20) — the bottleneck went distributed

Re-profiled single-writer insert on the merged compression build (`8226bfcb`,
debug-symbol, `sample`). The pre-compression profile was dominated by WT
disk-machinery (reconcile/eviction/block-write); **compression removed that**, and
the profile is now **distributed with no single hotspot**: BSON write-path
round-trips (~577 samples) ≈ WT btree/eviction ops (~570 combined), plus zlib
`deflate` (~77, the compression-CPU trade). Implications for closing the residual
~7×:
- **BSON write path:** the earlier "skip storage-encode when `_id` present" A/B
  measured **~0** (WT dominated then). Post-compression a win needs the *full* raw
  path — peek `_id` from raw input, store input bytes unchanged, AND splice raw
  bytes into the oplog `o` (restructuring `emit_oplog`, which touches the
  change-stream path — real risk). Expected win modest (BSON is ~4% of samples).
- **WT-op count / scaling:** the natural-order index (1 extra WT write/doc vs
  mongod) and the shared-btree scaling limit are **structural** (on-disk format /
  RecordId model, Phase 5).
- **Conclusion:** a distributed profile means each remaining lever is a *small*
  win; the large gap is mongod's structural advantages (native C++ efficiency,
  fewer WT ops/doc, SBE). Closing it is the Phase-5 rewrite (per-collection tables
  + RecordId, killing the natural-order index) — genuinely multi-week, hard to
  reverse, and requiring new concurrent-capped + reopen/migration tests. That is
  the honest scope of "the last 7×"; the two shipped levers (config + compression)
  were the disk-volume fruit.

## ⭐ BREAKTHROUGH: the oplog is the dominant bottleneck (2026-07-20)

Isolated via an experiment toggle (`SECANTUS_NO_OPLOG`) — separate-collection
scaling, 15s, unloaded:

| writers | oplog ON | oplog OFF |
|---|---|---|
| 1 | 16,064/s (1.00×) | **46,283/s (1.00×)** |
| 2 | 0.92× | 1.18× |
| 4 | 0.68× | 1.94× |
| 8 | **0.60× (144k agg)** | **2.73× (1,898,800 agg)** |

**The oplog costs ~2/3 of single-writer throughput AND nearly all the scaling
collapse.** Oplog OFF: single-writer 16k→**46k** (2.9×; only 2.4× from mongod's
110k, down from 7×) and 8-writer scaling 0.60→**2.73×** (near mongod's 4.55×). So
the residual gap is NOT per-collection tables or MVCC locks — it is the **oplog**:
every write appends to one shared oplog btree tail (page-latch contention at the
monotonic-seq tail), takes the `oplog` mutex (seq/ts mint + condvar notify), and
writes a full-doc entry (a second ~8KB row). This is Phase 3, and it's the #1
lever (was mis-prioritized).

**Tractable oplog optimizations to pursue (each measured, gated by the
change-stream + concurrency suites):**
- Raw-BSON oplog entry: splice the doc's already-encoded bytes into `o` instead of
  re-encoding (single-writer).
- Cache the oplog/preimage cursors per-thread (if opened per-emit — the profile
  showed cursor-cache churn).
- Reduce `oplog` mutex hold / contention; consider mongod's optime-reserve +
  out-of-order-commit + holes model for the btree-tail scaling limit (the deep part).
The oplog-off numbers are the ceiling this phase chases: ~46k single-writer, ~2.7×
scaling — a genuine "approaching mongod" trajectory.

**Micro-experiments (both measurement-disproven, reverted — the loop caught them):**
- *Remove the per-emit second `oplog` mutex acquire* (the in-emit `notify_all` is
  redundant — the real wake is post-commit in `with_statement_txn:1803` — and move
  the prune counter to an atomic): **~0** (8w 0.60→0.64×, noise). So the `oplog`
  mutex is NOT the scaling limiter. *(The redundant-notify removal is still a valid
  cleanup for a future PR; it just isn't a perf lever.)*
- *Un-compress the oplog/preimage tables* (hypothesis: `deflate` CPU on transient
  rows): **WORSE** (single-writer 15.4→12.8k, 8w 0.64→0.36×). The oplog's cost is
  disk-write *volume*, not compression CPU — **compressing it was correct** (keep
  as merged).
- *WT oplog-table tuning* (`leaf_page_max=1MB`, `memory_page_max=8MB`,
  `access_pattern_hint=sequential` — hypothesis: fewer rightmost-page splits cut the
  append contention): **WORSE** (single-writer 16→11.7k, 8w 0.60→0.54×). Bigger
  pages add per-op cost without touching the mechanism. Reverted.

**All tractable oplog levers (mutex, compression toggle, WT page tuning) are
measurement-disproven** — the scaling collapse is NOT a config/tuning problem. The
only remaining fix is the deep **optime-reserve + out-of-order-commit + holes**
rearchitecture (mongod's oplog model), which decouples writers from the shared
oplog's commit/append ordering. That is genuinely multi-day, correctness-critical
(touches the oplog write path AND the tailable-cursor read path — readers must
track holes), and uncertain to fully recover the oplog-off ceiling — the honest
scope of the last ~7×. The two shipped levers (config + compression) plus this
precise, exhaustively-narrowed diagnosis are the session's deliverables.

**Therefore the residual oplog gap is structural, precisely located:**
- *Single-writer:* the oplog is a fundamental *second* compressed ~8KB write per
  insert (mongod does this too; mongod wins on raw C++/WT efficiency — the
  distributed-profile finding, not one lever).
- *Scaling:* the **shared oplog btree tail** — all writers append monotonic-seq
  keys to one rightmost page → WT page-latch serialization. This is THE scaling
  limiter. The fix is mongod's model: reserve optime atomically, commit oplog rows
  out-of-order in each writer's own txn, track "holes" so readers see a consistent
  prefix — so writers don't rendezvous on one page. That is the focused Phase-3
  work; the oplog-off ceiling (2.73× scaling) is what it can recover.

## ⭐⭐ THE FIX: sharded oplog (2026-07-20) — recovers most of the gap

The tractable single-table oplog tunings were all disproven; the fix is to **shard
the oplog across N btrees** so concurrent writers don't rendezvous on one table's
rightmost append page. Experiment: 16 write-shards (`secantus_oplog_sh{0..15}`,
routed by `start_seq % 16`), read path unchanged (bench-only measurement). Measured
(separate collections, 20s, unloaded):

| writers | baseline (1 table) | **16-shard oplog** | oplog-off ceiling |
|---|---|---|---|
| 1 | 16k/s (1.00×) | **28.9k/s (1.00×)** | 46k |
| 2 | 0.92× | **1.24×** | 1.18× |
| 4 | 0.68× | **1.98×** | 1.94× |
| 8 | **0.60×** | **2.47×** | 2.73× |

Sharding recovers **single-writer 16k→28.9k (1.8×)** and **8-writer scaling
0.60→2.47× (~4×)** — approaching the oplog-off ceiling and mongod's 4.55×. So the
gap to mongod drops to ~3.8× single-writer / ~1.8× scaling: genuinely "approaching."
(Single-writer improves too because one writer's consecutive batches rotate across
shards → far less per-page append churn.)

**Remaining work to ship it (the real Phase-3):** the read path must merge the N
shards in seq order for every oplog reader — the tailable change-stream producer,
`read_oplog`, `find_seq_for_ts`, `prune_oplog_inner`, recovery (`load_oplog_meta`),
and PITR replay. A k-way merge by seq (each shard is seq-sorted; a doc's shard is
`seq % N`, so a point lookup is O(1) too). Then validate against the FULL
`test_mongo_server_concurrency` + change-stream suites (change streams must see
every event, in order, with correct resume tokens across shards). This is
correctness-critical but now clearly justified — the win is proven.

## ⭐⭐⭐ RESULT (2026-07-20 PM): bounded prune — 1.77× single / 12.4× 8-writer (MEASURED)

The bounded-prune fix landed and was measured **back-to-back on one machine**
(baseline = main single-table + full-scan prune, new = sharded + bounded prune):

| | baseline (main) | new (sharded + bounded prune) | improvement |
|---|---|---|---|
| 1 writer | 13,198/s | **23,424/s** | **1.77×** |
| 8 writers (aggregate) | 3,533/s (0.27× scaling) | **43,617/s (1.86× scaling)** | **12.4×** |

This REVERSES the earlier sharding-only tradeoff: single-writer is now *better*
than baseline (not −20%), and 8-writer throughput is 12× higher. vs mongod:
single-writer 0.10×→**0.21×**, scaling 0.06×→**0.41×** of mongod's 4.55×. The
re-profile confirmed the mechanism: the prune scan (`__wt_btcur_next`) fell from
46% of dispatch CPU to 11%; the write path is now dominated by `__wt_txn_commit`
+ `__wt_btcur_insert` (real write work). Correctness: 103 Rust-server
integration tests (oplog/pitr/cross-server/change-stream/concurrency) + storage
cargo suite + fmt/clippy green.

Remaining to mongod: single-writer still ~4.7× off (now commit/journal-bound —
`__wt_txn_commit` dominates), 8-writer scaling 1.86× vs 4.55×. Next levers:
group-commit / journal batching (the commit cost), and the write-amplification /
double-encode items below.

## After the prune fix: the write path is LEAN — remaining gap is structural

Post-bounded-prune profile + code audit of the single-writer insert path: no
redundant work remains to cut cheaply.
- `_id_` is a **virtual** index (never stored), so index-free collections have an
  empty `descs` → `unique_conflict` and `write_index_entries` are no-ops. No
  redundant `_id` uniqueness probe (the doc-table `overwrite=false` insert enforces
  it).
- `with_statement_txn` is clean (begin / run / commit / post-commit notify); the
  batch `insert` already commits ONCE per `insertMany` (all docs in one WT txn), so
  the commit is already amortised across the batch.
- The hot costs are now inherent: `__wt_txn_commit` (2212) + `__wt_btcur_insert`
  (1046) + WAL I/O (`__posix_file_write`/`__log_fs_write`/`__posix_file_sync` ~1947)
  + `encode_doc` (595). `transaction_sync=(enabled=false)` already avoids a
  per-commit fsync.

**The one reducible structural item: write amplification = 4 WT writes/doc** — doc
(1) + natural-order index (2: `secantus_natural` seq→id_key + `secantus_natural_seq`
id_key→seq) + oplog (1). The 2 nat writes exist because docs are keyed by `id_key`
(the `_id` sort key), NOT by insertion order — so a separate index carries
`$natural` order + capped eviction, and its reverse map makes delete O(1).

**Next major lever (Phase 5, LARGE — user-sanctioned): RecordId-keyed doc tables.**
Key the doc table by a monotonic per-collection RecordId (insertion order) instead
of `id_key`, add an `_id → RecordId` secondary index. Then `$natural` order is the
doc-table order for free (drops BOTH nat tables), at the cost of one `_id`-index
write: 4 → 3 writes/doc (~25% amplification cut → est. ~15-20% single-writer, to
be MEASURED not projected). This is mongod's own catalog model. It's a big on-disk
format change (re-key every doc, rewrite all `_id` lookups, a migration) + the
Python mirror — a scoped standalone effort, not a tail-of-session change. The
smaller alternative (splice the pre-encoded doc into the oplog `o` field to avoid
the double encode) is only ~5% and risks the change-stream oplog format — poor
risk/reward, skipped.

## ⭐ PROFILE (2026-07-20 PM): the write-path bottleneck is the opportunistic PRUNE

`sample` of a single-writer insert loop (scratchpad/profile_insert.sh, load 1.27 —
clean) shows the worker thread's `run_dispatch → dispatch` spends **77% of its CPU
inside `emit_oplog`** (10,575 / 13,776 samples), and within that the dominant leaf
is **`__wt_btcur_next` — cursor iteration**, i.e. the every-1000-emits opportunistic
prune scanning the WHOLE oplog. Not the oplog write, not `index_descs` (my earlier
guess — WRONG), not the doc/nat writes. Profiling corrected the guess; this is why
the plan mandates measurement over projection.

The full-oplog prune scan is **pre-existing** (the single-table baseline does it too
— which is why baseline single-writer is also low) but sharding made it worse (my
prune materialized every blob via a 17-cursor merge + a full `seq→table` HashMap).

**Fix (profile-justified, helps single-writer on BOTH servers):** maintain a live
oplog entry count so the opportunistic prune doesn't walk the whole oplog:
- Under cap and oldest entry in-window → prune is ONE ts read (early-out), not a
  100k walk.
- Over cap → walk only the bounded excess (`live_count - max_entries`), oldest-first.
`live_count`: counted once on open, `+= n` per `emit_oplog` / import, `-= doomed`
per prune. Correctness-gated by the prune cap/retention tests + authoritative
recount on open. Being built now; push gated on the cooldown A/B.

## Next lever: oplog PER-WRITE cost (the single-writer gap) — diagnosed, not yet built

Sharding addressed *contention* (multi-writer) but not per-write *cost*, which is
why single-writer stayed ~11-16k vs mongod's ~110k and the oplog-OFF A/B ceiling of
46k. Reading `insert_one` (`crates/secantus-storage/src/lib.rs:2900`), each single
insert with the oplog on does, under the collection lock:

- **~4 WT writes**: doc table (1) + natural-order index (2: `secantus_natural` +
  `secantus_natural_seq`) + oplog (1). mongod is ~2 (doc + oplog).
- **~6 metadata cursor reads, every write**: `ensure_collection`, `is_timeseries`,
  `index_descs` (a scan of the index table), `unique_conflict`, `maybe_mark_multikey`,
  `collection_uuid`. These are catalog lookups that change only on DDL — mongod
  caches them; we re-read them per insert.
- **a double BSON encode of the document**: once as the doc-table value (`blob`),
  again when `emit_oplog` re-encodes the whole entry whose `o` field IS that same
  document.

Concrete levers, each its own **measured** PR (biggest first), to run on a clean
cool machine (this session's machine is thermally unmeasurable):
1. **Per-(db,coll) catalog cache** — cache `index_descs` / `collection_uuid` /
   `is_timeseries` / options, invalidated on create/drop/collMod/createIndexes.
   Removes ~5 cursor reads per write. Highest-value, but correctness-sensitive
   (stale-cache-after-DDL) → careful invalidation + its own test.
2. **Splice the pre-encoded doc into the oplog entry** — build the oplog `o` value
   from the already-encoded `blob` instead of re-encoding the moved `doc` (raw-BSON
   write-path analogue of Phase 1's read splice). Helps large docs.
3. **Natural-order de-amplification** — collapse the 2 nat-index writes toward 1
   (or fold the reverse map), reducing write amplification 4→3.

None of these are built. All need before/after `bench.concurrency` on an unloaded
machine — the plan's #1 rule (measure, don't project) is why they are NOT being
shipped blind here.

## ⚠️ CORRECTION (2026-07-20 PM): honest back-to-back A/B — it's a TRADEOFF

The earlier "single-writer 16k → 28.9k (1.8×)" headline **does not reproduce** and
was a measurement artifact (cooler machine / short-run noise). A rigorous
back-to-back A/B on ONE machine state (single-table baseline binary vs sharded
binary, same run) gives:

| | baseline (1 table) | sharded (16 btrees) | ratio |
|---|---|---|---|
| 1 writer | 13,525/s | 10,793/s | **0.80× — REGRESSION** |
| 8 writers (total) | 55,000 | 92,000 | **1.67×** |
| 8-writer scaling | 0.27× | 0.57× | improved |

**Sharding is a tradeoff, not a pure win:** it relieves multi-writer append
contention (8w throughput +67%, scaling 0.27→0.57×) but *hurts* single-writer ~20%
— one writer has no contention to relieve, so sharding only adds routing overhead
and scatters a batch's appends across several btrees instead of one cache-hot
page. Whether it's worth shipping depends on the target workload (concurrent
writers: yes; single-writer-dominated: no) — a **user decision**, not an
autonomous one, especially given it's a correctness-critical on-disk format change.

Caveat: the absolute numbers are thermally unreliable this session (baseline 8w
scaling measured 0.60× earlier, 0.27× now). The RATIOS (back-to-back, same run)
are the trustworthy signal; a clean cool-machine re-measure is still owed before
any headline number is quoted.

## Implementation status (2026-07-20): sharded oplog SHIPPED end-to-end

The full sharded-oplog read path is implemented in **both** storage layers and
green on the integrity suites:

- **Rust** (`crates/secantus-storage/src/lib.rs`): **per-batch** write routing
  (a whole `emit_oplog` batch → one shard by `start_seq % OPLOG_SHARDS`), a k-way
  `read_oplog_shards` merge (shards + legacy table), and every reader converted —
  `read_oplog`, `scan_max_oplog_seq`, `oplog_floor_seq`, `find_seq_for_ts`,
  `prune_oplog_inner` (tagged merge → delete from each seq's exact table),
  `load_oplog_meta` recovery, `archive_doomed_oplog` (probe all tables), and
  `import_oplog_segment` (restore → one shard).
  - **Per-batch, NOT per-entry — measured lesson.** The first cut routed per
    *entry* (`seq % N`) for O(1) point-lookups; it *regressed* single-writer to
    ~8.6k docs/s (below the 16k baseline) because scattering a 100-doc batch's
    contiguous seqs across all 16 btrees destroys sequential-append locality.
    Per-batch keeps each batch a contiguous append to one tree (locality) while
    concurrent writers spread across trees (scaling). The cost — a seq's shard
    isn't derivable from the seq — is paid by the k-way merge (ordered reads) and
    all-table probes (rare point-ops); the prune uses the merge's shard tag to
    still delete from exactly one table.
- **Python** (`src/secantus/storage.py`): the sharded oplog is an **on-disk
  format change**, so the Python server must READ/RECOVER/PRUNE a Rust-written
  store for cross-server PITR/backup (`test_rust_pitr_cross_server`). Python
  writes stay single-table (its global lock means sharding buys it nothing, and
  Rust's merge already reads the legacy table), but every Python oplog *reader*
  now merges shards + legacy (`_merge_oplog_on_session` + `_scan_max_oplog_seq` /
  `oplog_floor_seq` / `find_seq_for_ts` / `_prune_oplog_locked` / `_load_oplog_meta`
  / `_archive_doomed_oplog` / `_scan_oplog_entries`).

**Two bugs found + fixed during validation** (both would have shipped silent data
loss — exactly what the "this is a database" rule guards against):
1. *Cross-server format*: sharding the Rust oplog broke `oplog_floor_seq()` on the
   Python reader (it read the empty legacy table → "no oplog to replay"). Fixed by
   making every Python reader shard-aware.
2. *Overwrite-mode delete*: WT cursors are `overwrite=true`, so `remove()` of an
   absent key returns Ok — the "delete from shard, else fall back to legacy" logic
   never fell back, leaking pruned rows on a Python-written store. Fixed (both
   servers) by deleting from **both** shard and legacy unconditionally.

Gate status: Rust storage `cargo fmt`/`clippy`/`test` green (27 test binaries);
`test_mongo_server_concurrency` (Rust concurrency + change-stream integrity),
`test_rust_pitr_cross_server`, `test_pitr`, `test_oplog`, `test_change_streams`
all green. The oplog **visibility-hole** race (a lower seq committing after a
higher one is already visible) is unchanged in kind by sharding — the merge has
identical snapshot semantics to the old single-table scan — and did not manifest
under the concurrency suite; re-run repeatedly since sharding raises write
parallelism.

## Success criteria (mirrors the mongod numbers we're chasing)
- Write scaling on **distinct** collections: ≥ 2.5× at 4 writers, approaching
  mongod's ~4.1× at 8 (bounded by whatever Phase 1 shows WT can actually do here).
- Same-collection scaling ≥ 1.5× at 2 writers (no lost updates, unique races correct).
- No sub-1× collapse at 8 writers (admission control holds it flat-or-up).
- Full concurrency integrity suite + gauges + parity green throughout.
- Single-client latency non-regressing (stay at the post-raw-BSON 1.5×–2.5×).
