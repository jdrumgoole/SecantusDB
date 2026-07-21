# Rust server → mongod write performance: state & forward plan

Status: **active** (2026-07-21). Consolidates the measured findings of the
sharded-oplog / bounded-prune / doc-sharding / RecordId / WAL investigation so
future work builds on data, not guesses. The exhaustive session log lives in
`tasks/rust-mongodb-parity-redesign.md`; this doc is the **actionable forward
plan**.

Goal (Joe): "redesign the rust server so it approaches MongoDB performance." The
storage engine under both is the same WiredTiger 7.0.33, so every gap lives
*above* WT.

---

## 1. Where we are (measured, clean idle machine, 20s steady-state)

| axis | Rust server (post #594) | mongod | gap |
|---|---|---|---|
| single-writer | ~22k docs/s | ~110k | **5×** |
| 4-writer aggregate | ~56k docs/s | ~185k | ~3.3× |
| 8-writer aggregate | ~44k docs/s | ~500k | ~11× |
| scaling (8w vs 1w) | ~1.9× | 4.55× | — |

**Shipped this cycle (both merged, measured, on `main`):**

- **#582 — sharded oplog + bounded prune.** The opportunistic retention prune was
  re-scanning the *whole* oplog on every write (77% of single-writer CPU by
  profile). Bounded it with a live entry count (early-out under cap; bounded
  doomed-walk otherwise) and sharded the oplog across 16 btrees. **Measured:
  single-writer 1.5×, 8-writer scaling 0.27→~1.9×.** The prune fix was the big win.
- **#594 — doc-table sharding.** `secantus_documents` split into 16 per-collection
  WT tables (FNV-1a hash of `(db,coll)`, byte-identical in Rust + Python) so
  concurrent writers to different collections hit different WT files → different
  block-manager locks. **Measured: +19% aggregate throughput at 4 writers, +11% at
  2** (neutral at 1, −2% at 8 where the workload is I/O-bound).

---

## 2. Proven findings — DO NOT re-investigate (data attached)

Every one of these cost a measured experiment this cycle. Re-running them wastes
days.

1. **The write path is lean after the prune fix.** Profile of single-writer insert:
   `__wt_txn_commit` (~33%), `__wt_btcur_insert` (~16%), WAL I/O (~29%),
   `encode_doc` (~9%). No redundant work: `_id_` is a *virtual* index (empty
   `descs` for index-free collections → `unique_conflict`/`write_index_entries` are
   no-ops); the batch insert already commits once per `insertMany`.
2. **Write amplification is 4 WT writes/doc** (doc + 2 natural-order + oplog).
   A prototype dropping 1 write (→3) measured **+15% at 4-8 writers, FLAT
   single-writer.** So fewer writes helps *concurrency* (contention/I/O relief),
   not single-writer (which is commit/WAL-bound, not write-count-bound).
3. **The WAL is a +42% single-writer ceiling** (log-off: 22k→30k). BUT **log
   compression BACKFIRES (−35%)** — the WAL bottleneck is write-syscall/flush/commit
   *overhead*, not byte volume (fast SSD), so compressing only burns CPU. The one
   obvious durable WAL lever is a dead end.
4. **Even WAL-free, single-writer is 3.7× off mongod** — so ~70% of the
   single-writer gap is inherent WT/commit/encode CPU (the C++-efficiency gap), not
   anything a config or small change touches.
5. **Doc-shard COUNT is irrelevant** (8 ≈ 16 ≈ 32 all give +19% at 4w). 16 chosen
   for collision-avoidance.
6. **Dead ends from earlier in the cycle:** per-*entry* oplog routing (destroys
   sequential-append locality, −45% single-writer vs per-batch); cache auto-sizing
   (a short-run burst artifact, flat at 20s steady-state); the in-process
   selectable-engine model (retired for two separate servers).

---

## 3. The frontier — remaining levers, ranked by reward ÷ effort

### Lever A — RecordId-keyed doc tables  *(the one confirmed remaining lever)*
- **Reward: +15% at 4-8 writers (MEASURED via prototype). Flat single-writer.**
- **Effort: multi-day. Risk: HIGHEST (re-keys core doc storage — a bug is data
  corruption).**
- **What it is:** key the doc table by a monotonic per-collection RecordId
  (insertion order) instead of `id_key`. Then `$natural` order *is* the doc-table
  order for free — drop the `secantus_natural` (seq→id_key) forward table (that's
  the 4th write). Reuse the existing `next_nat_seq` counter as the RecordId, and
  reuse `secantus_natural_seq` (id_key→seq) as the `_id` index.
- **Blast radius (measured):** 148 `id_key` usages in Rust storage + every
  secondary index entry embeds `id_key` as its fetch pointer (`pack_entry` =
  sortkey + `\0\0` + id_key) → the on-disk index-entry format changes to carry
  RecordId, rippling through all index maintenance + the IXSCAN fetch path. Same
  again in Python. Plus a migration.
- **Incremental build order (gate + measure each stage):**
  1. Doc table keyed by RecordId + `_id` index (id_key→RecordId); rewrite the
     per-`_id` paths (find/update/delete/replace/upsert) to go id_key → `_id` index
     → RecordId → doc. Natural-order scans walk the doc table directly.
  2. Secondary index entries store RecordId instead of id_key (format change);
     update `pack_entry`/`unpack_entry`, all index maintenance, and the IXSCAN
     fetch. This is the biggest sub-step.
  3. Capped-collection eviction + `$natural` hint (doc-table order).
  4. Python mirror (same paths, byte-identical RecordId scheme).
  5. One-time on-open migration: re-key existing id_key-keyed docs to RecordId +
     build the `_id` index.
- **Correctness gates:** the full `tests/test_mongo_server_concurrency.py`
  integrity suite, `test_indexes.py` (index fetch via RecordId), `test_crud.py`
  (all `_id` paths), reopen/PITR/backup round-trips, `rust-gate`, the pymongo +
  cross-driver gauges. A single wrong id_key→RecordId hop is silent data loss.
- **Recommendation:** worth it ONLY if multi-writer concurrency throughput is a
  hard priority and a multi-day, corruption-risk core rewrite is acceptable. The
  prototype proves the +15%; the risk/effort is the real question.

### Lever B — single-writer WT/commit/encode  *(low reward, largely inherent)*
- **The WAL (+42% ceiling) resists durable optimization** (finding 3). The
  remainder is inherent WT per-op + C++ efficiency (finding 4).
- Speculative, unmeasured, uncertain: WT log-slot buffer tuning (fewer, larger log
  writes — not exposed cleanly in the config string); the oplog `o`-field
  double-encode splice (~5%, risks the change-stream oplog format).
- **Recommendation: do NOT pursue for the reward.** Single-writer parity is not
  reachable via config or small changes; it would need a different execution model
  (streaming/SBE-lite, Lever C) with a large lift and no guarantee.

### Lever C — streaming / SBE-lite aggregation & write path  *(largest, research)*
- Replace the `Vec<Document>`-between-stages model + owned-BSON materialization
  with a pull-based, typed-slot pipeline (mongod's SBE analogue). Addresses the
  inherent per-op CPU that bounds single-writer *and* read/aggregate latency.
- **Effort: very large (a `secantus_core` execution-model rewrite). Reward:
  potentially the only thing that moves the inherent gap, but unproven.** A
  dedicated research project, not an incremental PR.

---

## 4. Cross-cutting: fix the measurement environment FIRST

This cycle was repeatedly blocked by **thermal throttling** (hours of continuous
builds/benchmarks depressed throughput ~10×) and **parallel-session CPU load**
(other worktrees kept the box saturated for hours). Multiple measurements were
invalid until caught.

**Before any further perf work:** establish a clean measurement path — a pinned
detached worktree at a SHA (per `CLAUDE.md`), a cool + quiet machine (pause
parallel sessions), and a self-verifying gate that confirms a known-good binary
hits its true throughput before trusting an A/B. The zlib-WT build gotcha
(standalone binaries need a WT built with `-DHAVE_BUILTIN_EXTENSION_ZLIB=ON`; all
cached WT builds are stale) is documented in the `standalone-binary-needs-zlib-wt`
memory.

---

## 5. Recommended sequence

1. **Bank #582 + #594** (done — merged). These are the cycle's deliverables:
   real, measured concurrency wins.
2. **If pursuing further:** set up a clean measurement environment (§4), then
   **Lever A (RecordId keying)** as a scoped, incremental, corruption-gated
   multi-day project for the confirmed +15% concurrency.
3. **Do not** chase single-writer via config/WAL (§3B, proven dead end) — it needs
   Lever C (a research-scale execution-model rewrite) or nothing.
4. Re-baseline against a current `mongod` on the same box before quoting any new
   gap number — the ratios here are pinned to this cycle's runs.
