# RecordId keying — incremental implementation plan

**Goal:** cut write amplification from **4 WT writes/doc → 3** by keying the doc
table on a monotonic per-collection **RecordId** (the existing nat-seq) instead of
`id_key`. Measured earlier at **+15% concurrency** (prototype); the clean re-measure
(2026-07-23) confirms the concurrency gap is the dominant one, so this is the real
lever. **Highest-risk change in the codebase — a wrong `id_key→RecordId` hop is
silent data loss.** Build incrementally; each step is its own gated PR.

## HANDOFF — current state (2026-07-23)

**Branch `rust-recordid-step1` (worktree `../SecantusDB-recordid`).**

### ✅ Step-1 storage layer COMPLETE — full storage crate green
Option 2 (id_key framed into the doc-table value) is implemented and the **entire
`secantus-storage` crate test suite passes** (32/32 lib + every integration suite:
crud / batch_insert / lifecycle / ttl / capped / concurrent_reads / durable-reopen /
archive / restore / oplog-prune). `cargo fmt --check` + `cargo clippy --all-targets
-- -D warnings` clean.

What landed (all in `crates/secantus-storage/src/lib.rs`):
- `frame_doc_value(id_key, blob)` / `unframe_doc_value(value)` — value framed as
  `[u32-LE id_key_len][id_key][blob]`; `type ScannedDoc = (i64, Vec<u8>, Vec<u8>)`.
- `scan_docs` walks the doc table (SSq) directly and returns `(RecordId, id_key,
  blob)` via unframe — natural (insertion) order, no `_id` decode, timeseries-safe.
  `scan_docs_natural` DELETED; `scan_blobs`/`scan_blobs_natural` delegate to it.
- Every doc-table WRITE frames the value + keys by RecordId (`set_key_ssq`):
  insert_one, batch insert, replace_by_id, update (doc stays at its RecordId), upsert
  (mint RecordId FIRST), rename/clone (re-mint per doc in src natural order).
- Every doc-table READ/DELETE resolves `id_key → doc_recordid → RecordId` then
  unframes: find_by_id, scan_collection, candidate_docs (index-routed fetch),
  docs_by_id_keys (IXSCAN), delete_by_id, delete_matching, prune_ttl,
  enforce_capped_bounds (all keep "doc row first, entries after" — recordid read via
  read-only `doc_recordid`, `_id`-index row dropped last by `delete_nat_entry`).
- `scan_max_nat_seq` now scans the doc **shards** for the max RecordId (the forward
  `NAT_TABLE` it used is gone) so `next_nat_seq` recovers correctly on reopen — this
  fixed the reopen/restore WT_DUPLICATE_KEY + zero-count failures.
- `migrate_legacy_docs` (pre-shard single-table path) re-frames + RecordId-keys +
  writes the `_id` index.
- **Behaviour change**: `scan_collection` now returns **insertion (RecordId) order**,
  not `_id` order — this matches mongod's natural order and what `find()` already
  returned via `scan_blobs_natural`. Test `scan_is_in_cross_type_natural_order`
  renamed → `scan_is_in_natural_insertion_order` and updated.

### Remaining before merge (wider gates — NOT yet run)
- `./inv rust-gate` (clean workspace fmt/clippy/test — secantus-core etc. unchanged,
  should be green) + `./inv rust-server-build` (needs `vendor/wiredtiger` submodule
  checked out in the worktree — was being inited).
- `./inv validate --server rust` (pymongo gauge, in a sub-agent) — must stay
  non-regressing.
- `tests/test_mongo_server_concurrency.py` integrity suite against the rust server.
- A note for a real in-place upgrade of a **pre-PR sharded beta store** (doc shards
  keyed SSu, unframed): WT fixes key_format at create time so those rows are NOT
  auto-migrated by the current code (no test covers it; the fresh-code reopen tests
  all pass). If we ship this to beta users mid-stream, add a shard-generation
  migration. → add to `tasks/backlog.md` §7 before merge.

### Prior foundation notes (superseded by the above)
Core write + `_id` point-read of RecordId keying worked at 23/32; the design fork is
now resolved and implemented — see "✅ DESIGN FORK — DECIDED" below for the design.

### Done (in `crates/secantus-storage/src/lib.rs`)
- `DOC_TABLE_CFG` key_format `SSu` → `SSq` — doc table keyed by RecordId (i64), not
  id_key. (Comment near the const.)
- `write_nat_entry` → mints the RecordId, writes ONLY the `_id` index row
  (`natural_seq`: id_key → RecordId) with `overwrite=false`, returns the RecordId.
  **The forward `NAT_TABLE` (seq → id_key) write is dropped** (the 4th write gone).
  **Dup-`_id` detection MOVED here** (was the doc-table insert; the doc table now
  keys by the unique RecordId so it can't reject dups). Returns
  `StorageError::DuplicateId` on dup.
- `doc_recordid(session, db, coll, id_key) -> Option<i64>` — the `_id`-index
  resolver (new helper).
- `delete_nat_entry` → returns `Option<i64>` (the RecordId) so the caller deletes
  the doc-table row; no longer touches NAT_TABLE.
- `insert_one` + batch `insert` — restructured: `write_nat_entry` (mint RecordId +
  `_id` index, catch dup) FIRST, then doc-table insert keyed by RecordId
  (`set_key_ssq`). Dup → `DuplicateId` / 11000 write-error. WT_DUPLICATE_KEY does
  not abort the txn, so unordered inserts still continue.
- `find_by_id` — `id_key → doc_recordid → RecordId → doc`.

### Remaining (the 9 failing tests pinpoint each path)
| failing test | path to convert |
|---|---|
| `insert_stores...verbatim` | `scan_collection` / `scan_docs` (doc-table walk `SSu`→`SSq`) |
| `capped_collection_evicts...` | `scan_docs_natural` + `enforce_capped_bounds` |
| `partial_index_used...` | IXSCAN fetch: index-entry `id_key` → RecordId → doc (`_docs_by_id_keys` / `_candidates_iter`) |
| `durable_close_roundtrips`, `replay_rebuilds`, `create_archive...`, `v2_restore...`, `fast_close...`, `opportunistic_prune...` | **on-open migration** of legacy `id_key`-keyed docs → RecordId + build the `_id` index; the reopen path |

Also (not yet touched, no dedicated failing test but required): `update_matching_core`
/ `delete_matching` / upsert / replace `_id`-resolution (sites ~3577, 3665, 5221,
6282, 6369 call `write_nat_entry`/`delete_nat_entry` — their return values (RecordId)
must now drive the doc-table op, and their doc-table `set_key_ssu` → `set_key_ssq`);
`rename`/`drop`/`clone` doc-table sites; `drop_nat_collection` (drop only
`natural_seq` now). Full doc-table cursor list (21 sites): `grep -n doc_table_for`.

### ✅ DESIGN FORK — DECIDED (2026-07-23, Joe): option 2 (store id_key in the doc value)
Dropping the `seq → id_key` forward table means a doc-table walk yields
`(RecordId, blob)` with **no stored id_key**. For normal docs `id_key` is
reconstructable from `_id`, but **timeseries collections suffix the id_key**
(`id_key + nanos + counter`, `timeseries_doc_suffix`, NOT derivable from `_id`), so
a walk can't recover the suffixed id_key needed to delete a timeseries doc's
`_id`-index + secondary-index entries (capped eviction / scan-delete). **Chosen:
store the id_key IN the doc-table value alongside the blob.** (Rejected: option 1 =
a lean RecordId→id_key map, reintroduces the table we dropped; option 3 = documented
timeseries limitation.)

**Implementation of option 2:**
- Doc-table `value_format` stays `u` (opaque bytes) — no WT schema change; the
  framing is in-band in *our* encode/decode. Frame the value as
  `[u32-LE id_key_len][id_key bytes][blob bytes]`.
- Add `frame_doc_value(id_key, blob) -> Vec<u8>` and
  `unframe_doc_value(value) -> (&[u8] id_key, &[u8] blob)`. **Every doc-table write
  frames; every read unframes.** `find_by_id` / point reads return `blob` only
  (unframe, drop id_key); scans return `(RecordId, id_key, blob)` straight from the
  unframe — **no `_id` decode needed**, and it works for timeseries (the exact
  suffixed id_key is stored). This also removes the "reconstruct id_key from _id"
  complexity from every scan — a net simplification.
- Migration must re-write each legacy doc's value in the framed form (it has the
  legacy id_key = the old doc-table key).
- Cost: +~4 bytes + id_key length per doc (id_keys are short); zlib block
  compression absorbs most of it. Value stays a single `u` column so index/oplog
  machinery is untouched.

### How to resume
```
cd ../SecantusDB-recordid            # the worktree
export SECANTUS_WT_LIB=/tmp/wt-zlib SECANTUS_WT_INCLUDE=/tmp/wt-zlib/include \
       LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib
cd crates/secantus-storage && cargo test --release   # 23/32 now; drive the rest to green
```
**The compiler does NOT catch key-format mismatches** (WT validates `SSu`/`SSq` at
runtime) — the test suite is the checklist. Changing the scan return type to
`(RecordId, id_key, blob)` DOES give compiler errors at every caller (tuple arity),
which is the efficient way to find them. After storage-crate green: `./inv
rust-gate`, `./inv rust-server-build` + `./inv validate --server rust`, the
`tests/test_mongo_server_concurrency.py` integrity suite, reopen/PITR/backup
round-trips, cross-driver gauges. Then a **clean-machine A/B** (check load < 4 +
orphaned shells first — [[orphaned-claude-shells-eat-cpu]]) to confirm the +15%.

## Current layout (what we're changing)
- Doc table `table:secantus_documents*` key `SSu` = `(db, coll, id_key)` → `bson`.
  `id_key = sortkey::encode_value(_id)`.
- `secantus_natural` (`SSq` = `(db,coll,seq)` → `id_key`) — the forward nat-order
  table. **This is the write we drop.**
- `secantus_natural_seq` (`SSu` = `(db,coll,id_key)` → `seq`) — reverse. **Becomes
  the `_id` index.**
- Index entries: `pack_entry(kb, id_key)` = `escape(kb) + \x00\x00 + id_key`; IXSCAN
  fetch does `id_key → doc`.
- 4 writes/doc: doc + natural + natural_seq + oplog.

## Target layout
- Doc table key `SSq` = `(db, coll, RecordId)` → `bson`. RecordId = nat-seq
  (monotonic per collection; already minted lock-free via `next_nat_seq`... actually
  under the oplog mutex on main — see the parked `rust-oplog-lockfree` for the
  atomic version; keep as-is for step 1, revisit).
- `secantus_natural_seq` = `(db,coll,id_key)` → `RecordId` — the `_id` index.
- `secantus_natural` (forward) **dropped**.
- 3 writes/doc: doc(by RecordId) + `_id`-index + oplog.

## Step 1 (this PR) — doc table by RecordId + `_id` index + migration
Keep the **index-entry format unchanged** (still `id_key`); IXSCAN fetch becomes
`id_key → _id index → RecordId → doc` (one extra hop, optimised away in step 2).

**Measured cost of that hop: +14.7% on `find_indexed_range`** (7.14 ms → 8.19 ms).
See "Measured — step 1" below; that is the number step 2 has to give back.

Concrete edits (`crates/secantus-storage/src/lib.rs`):
1. Doc-table key: every `doc_cur.set_key_ssu(db,coll,id_key)` / `get_key_ssu` on the
   doc table → `set_key_ssq(db,coll,recordid)`. Doc-table CFG `key_format=SSu`→`SSq`.
2. `insert` / `insert_one`: mint RecordId (nat-seq), write doc by RecordId, write
   `_id` index (`natural_seq`: id_key→RecordId). **Drop the `secantus_natural`
   (forward) write** (`write_nat_entry` loses its NAT_TABLE insert).
3. `find_by_id`: `id_key → natural_seq → RecordId → doc`.
4. `scan_docs_natural`: walk the **doc table directly** (SSq = RecordId order); drop
   the NAT_TABLE indirection. `$natural` hint + capped eviction ride this.
5. `update_matching_core` / `delete_matching` / upsert / replace: resolve `_id`→
   RecordId via the `_id` index before touching the doc table.
6. IXSCAN fetch (`_docs_by_id_keys` etc.): `id_key → _id index → RecordId → doc`.
7. **Migration on open** (like `migrate_legacy_docs`): for each legacy id_key-keyed
   doc, look up its seq from the existing `secantus_natural`/`natural_seq` (or
   assign fresh in scan order), re-write the doc under `(db,coll,RecordId)`, ensure
   the `_id` index row, delete the old id_key-keyed row. Idempotent; runs once.

**Gates before merge:** `tests/test_mongo_server_concurrency.py` integrity suite,
`test_crud.py` (all `_id` paths), `test_indexes.py` (IXSCAN fetch), reopen / PITR /
backup round-trips, the storage crate's reopen + capped + nat-order tests,
`rust-gate`, pymongo + cross-driver gauges. `./inv rust-server-build` + gauge.

## Step 2 — index entries store RecordId (on-disk format change)
`pack_entry(kb, RecordId)`; `unpack_entry`; all index maintenance; IXSCAN fetch
`RecordId → doc` (drops the extra hop). Migration re-packs existing index entries.
The biggest sub-step. Own PR + gates.

**Target — recover the hop:** `find_indexed_range` should return to **≈7.1 ms**
(pre-RecordId) from step 1's **8.19 ms**; anything ≥ 7.6 ms means the hop is still
being paid somewhere. Verify with the same A/B recipe below (`--no-mongod --reps 9`,
this commit vs its parent) — and keep step 1's *gains* (scan / aggregate / delete,
below), which come from the doc table being in RecordId order and must not regress
while chasing the read number.

## Step 3 — capped-collection eviction + `$natural` hint on doc-table order.
## Step 4 — Python mirror (`src/secantus/storage.py`), byte-identical RecordId scheme.
## Step 5 — folded into step 1's migration if landable, else a dedicated pass.

## Measurement
Clean idle-machine A/B (load < 4; check for orphaned shells first —
[[orphaned-claude-shells-eat-cpu]]) at 1/2/4/8 writers, per step, vs the parent
commit. Expect +15% concurrency by the end.

### Measured — step 1 (2026-07-24)
A/B of `b90b5490` (step 1) vs its parent `397b03aa`, both built and run **back to
back in one detached worktree** on an idle machine (load < 2 before each leg;
`./inv compare-servers --no-mongod --reps 9`, n=10000). Rust-server medians:

| workload | parent | step 1 | Δ |
|---|---|---|---|
| **find_indexed_range** | 7.14 ms | **8.19 ms** | **+14.7%** ← the extra hop |
| find_all (scan) | 20.28 ms | 17.71 ms | −12.7% |
| aggregate `$group` | 12.55 ms | 10.44 ms | −16.8% |
| aggregate multistage | 17.45 ms | 15.36 ms | −12.0% |
| delete_many_half | 32.51 ms | 29.89 ms | −8.1% |
| insert | 82.07 ms | 79.55 ms | −3.1% |
| update_many_half | 49.89 ms | 51.39 ms | +3.0% (noise) |

**Noise floor ≈ ±3%**, established by the *Python* server in the same runs: step 1
is Rust-only, so Python is an untouched control and it moved 0.7–2.7% across the
two legs. That is what makes +14.7% a real regression and +3.0% not.

Net: step 1 is **positive overall** — everything that walks the doc table
sequentially (scan, both aggregates, delete) got 8–17% faster because the table is
now in RecordId (insertion) order; only the indexed-read path pays the hop.

Against `mongod` the Rust server measured **1.4×–2.6×** after step 1 (was 1.5×–2.5×
at the 2026-07-20 baseline): writes improved, `find_indexed_range` moved 1.5× → 1.8×.
**`docs/benchmark.md` and the other published figures were NOT refreshed** — step 2
moves the read number again, so publish once after it lands rather than twice (the
numbers live in five places that must agree — [[benchmark-numbers-alignment]]).
