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

### Phase 2 — Drop the per-collection CRUD lock → WT-MVCC-native *(biggest same-collection lever)*
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

## Success criteria (mirrors the mongod numbers we're chasing)
- Write scaling on **distinct** collections: ≥ 2.5× at 4 writers, approaching
  mongod's ~4.1× at 8 (bounded by whatever Phase 1 shows WT can actually do here).
- Same-collection scaling ≥ 1.5× at 2 writers (no lost updates, unique races correct).
- No sub-1× collapse at 8 writers (admission control holds it flat-or-up).
- Full concurrency integrity suite + gauges + parity green throughout.
- Single-client latency non-regressing (stay at the post-raw-BSON 1.5×–2.5×).
