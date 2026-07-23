# Rust server → mongod write performance: state & forward plan

Status: **active** (2026-07-21). Consolidates the measured findings of the
sharded-oplog / bounded-prune / doc-sharding / RecordId / WAL investigation so
future work builds on data, not guesses. The exhaustive session log lives in
`tasks/rust-mongodb-parity-redesign.md`; this doc is the **actionable forward
plan**.

Goal (Joe): "redesign the rust server so it approaches MongoDB performance." The
storage engine under both is the same WiredTiger 7.0.33, so every gap lives
*above* WT.

## UPDATE (2026-07-23): the 2026-07-22 VERDICT below was measured under hidden CPU contention — partly overturned

Two things invalidate the framing of the VERDICT immediately below. Read this first.

### 1. The "thermal throttling" was ~24 orphaned Claude Code shells, not heat
Every absolute number in the 2026-07-22 VERDICT (and much of §1–§3) was taken on a
box that had **leaked `zsh` shell-snapshot processes** (PPID 1, from prior/parallel
sessions, up to 26h old) spinning at ~50% CPU each — **11 of 12 cores gone, load
average ~40.** They accumulated in groups over the session, so contention *varied*,
which is why some A/B rounds read clean and others "throttled." Detection + kill
recipe is in the `orphaned-claude-shells-eat-cpu` memory (`uptime`; kill PPID-1
`shell-snapshots/snapshot-zsh` procs). After clearing them, **load fell 40→~3 and
the single-writer bench baseline rose from ~13k to ~28k.**

**Consequences for this doc's numbers:**
- The **true idle single-writer baseline is ~25–28k**, not the 22,479 in the VERDICT
  table. Ratios from *back-to-back same-contention* A/B rounds are roughly OK, but
  every **absolute** figure and any cross-run comparison here is suspect.
- The clean rust-vs-mongod multiple is therefore **smaller than the stated 5–6×**
  (both sides were contention-suppressed by different amounts at different times).
  Re-run the direct A/B on a verified-idle box (load < 4) before trusting any gap
  figure.

### 2. A per-op lever DID land — the VERDICT's core claim is wrong
The VERDICT says "every avenue was tried… none touches the per-op multiple; parity
is a property of the execution engine, not a lever we can pull." **Falsified.** The
**raw-BSON write path shipped as #608** (2026-07-23, on `main` as `aa578dcc`) and
measured, on a **verified-idle** box (load ~3, 4 alternating rounds):

| writers | baseline | raw-write | Δ |
|---|---|---|---|
| 1 | 25,131 | 27,863 | **+10.9%** |
| 4 | 59,709 | 63,183 | **+5.8%** |
| 8 | 65,431 | 64,925 | −0.8% (flat) |

It removes 3 of the 5 per-document BSON ser/de round-trips (merge-decode →
crud `encode_doc` → storage `decode_doc` → storage `encode_doc` → oplog encode):
the server diverts an insert's kind-1 `documents` un-decoded to the handler, which
passes the client's bytes straight to storage, stored **verbatim** when `_id`
leads. This is a genuine per-op efficiency win *above* WT — exactly the kind the
VERDICT said didn't exist. It goes flat at 8 writers because the insert workload
becomes **WAL/disk-bound** at high concurrency, so the saved CPU stops mattering.

### 3. Threads ARE effective (corrects §-narrative)
Un-throttled (contention cleared), the server uses **~3.8 cores at 4 writers, ~4.5
at 8**, and scales 1→8 writers with the same sublinear shape as mongod (both flatten
~4w). The earlier "37% CPU / threads blocked on eviction" reading was the contention
artifact, not a threading or eviction bug.

### 4. Levers re-scored on clean measurement
- **Raw-BSON write path — SHIPPED (#608), +11%/+6%/flat.** The real win of this cycle.
- **Raw oplog `o` splice ("increment 3")** — built + fully validated (gauge unchanged
  99.5%, change streams 106/0/100%, `fullDocument` byte-identical) but clean A/B
  measured **~+1% = noise**: the oplog write is **WAL/disk-bound, not encode-bound.**
  **DROPPED**; parked on branch `rust-raw-oplog-splice`. Do not re-chase unless the
  WAL itself is first made cheaper.
- **WT cache size (auto-size like mongod)** — **+6% at 4w, minor** (measured; NOT the
  "burst artifact" the old finding-6 claimed, but also NOT the gap; a CPU-utilisation
  proxy over-predicted it — throughput is the only metric that counts). Low-risk,
  worth shipping + fixing benchmark fairness, not a parity move.
- **RecordId keying (Lever A)** — still the standing +15%-concurrency lever, still
  highest-risk; re-evaluate its reward *after* a clean baseline, since a right-sized
  cache + the raw-write path change the eviction/ser-de picture it was relieving.

### 5. Standing lesson
**Never trust a perf number on this Mac without `uptime` (load < 4) + an orphaned-
shell check first.** The whole "parity unreachable" conclusion leaned on numbers a
contaminated machine produced.

---

## VERDICT (2026-07-22): mongod write-parity is NOT reachable for this architecture

> **⚠️ Superseded in part — see the 2026-07-23 UPDATE above.** Measured under hidden
> CPU contention; the absolute numbers are suppressed and claim #2 (no per-op lever
> exists) is falsified by shipped #608. Kept below for the reasoning and the
> still-valid *shape* observations (both servers peak ~4w), not the absolute gap.

A clean, **direct side-by-side** rust-vs-mongod A/B (both on this 12-core box, same
run, un-throttle-verified) settles it:

| writers | Rust (main, post #582+#594) | mongod | gap |
|---|---|---|---|
| 1 | 22,479/s | 112,274/s | **5.0×** |
| 2 | 27,016/s | 188,637/s | 7.0× |
| 4 | 40,989/s | 251,557/s | **6.1×** |
| 8 | 29,500/s | 169,322/s | 5.7× |

Two decisive observations:
1. **The gap is a flat ~5–6× at every writer count** — the signature of a
   *per-operation efficiency* gap (mongod executes each write ~5–6× cheaper), NOT a
   scaling/contention problem. Contention fixes (sharding, RecordId) cannot close a
   flat per-op multiple; RecordId's measured +15% moves 6.1×→~5.3× at best.
2. **Both servers share the same scaling shape** — both peak at 4 writers on 12
   cores (Rust 1.82×, mongod 2.24×) and *drop* at 8. So the earlier "mongod scales
   4.55× at 8w" figure does NOT hold on this machine; the whole story is the per-op
   multiple, which is inherent to mongod's decade-tuned C++ storage vs a
   Rust-over-WiredTiger surrogate.

Every avenue in §3 was tried and measured to its ceiling; none touches the
per-op multiple. **Parity is a property of the execution engine, not a lever we can
pull.** The session's wins still doubled single-writer (Phase-0 baseline 10,250 →
22,479, ~2.2×) — real, shipped, and worthwhile — but parity is out of reach.

---

## 1. Where we are (measured, clean idle machine, 20s steady-state)

Superseded by the direct A/B in the VERDICT above; original approximate table kept
for the per-writer aggregate context:

| axis | Rust server (post #594) | mongod (direct A/B) | gap |
|---|---|---|---|
| single-writer | 22,479 docs/s | 112,274 | **5.0×** |
| 4-writer aggregate | 40,989 docs/s | 251,557 | **6.1×** |
| 8-writer aggregate | 29,500 docs/s | 169,322 | 5.7× |
| scaling (peak @ 4w) | 1.82× | 2.24× | same shape |

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
