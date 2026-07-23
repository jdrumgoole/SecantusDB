# RecordId keying — incremental implementation plan

**Goal:** cut write amplification from **4 WT writes/doc → 3** by keying the doc
table on a monotonic per-collection **RecordId** (the existing nat-seq) instead of
`id_key`. Measured earlier at **+15% concurrency** (prototype); the clean re-measure
(2026-07-23) confirms the concurrency gap is the dominant one, so this is the real
lever. **Highest-risk change in the codebase — a wrong `id_key→RecordId` hop is
silent data loss.** Build incrementally; each step is its own gated PR.

## HANDOFF — current state (2026-07-23)

**Branch `rust-recordid-step1` (worktree `../SecantusDB-recordid`).** Foundation of
step 1 is implemented + tested: **23/32 storage crate tests pass** (WIP commit
`1a195595`; this doc committed after). Core write + `_id` point-read of RecordId
keying work. **Paused deliberately on a design fork (see "⚠ DESIGN FORK" below).**

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

### ⚠ DESIGN FORK — must decide before the scans/eviction
Dropping the `seq → id_key` forward table means a doc-table walk yields
`(RecordId, blob)` with **no stored id_key**. For normal docs, reconstruct
`id_key = encode_value(_id_from_blob)`. **But timeseries collections suffix the
id_key** (`id_key + nanos + counter`, `timeseries_doc_suffix`) so duplicate `_id`s
coexist — and the suffix is NOT derivable from `_id`. So a doc-table walk can't
recover the suffixed id_key needed to delete a timeseries doc's `_id`-index +
secondary-index entries (capped eviction, scan-based deletes). Options:
1. **Keep a lean `RecordId → id_key` map for timeseries collections only** (or
   always) — a partial reintroduction of the forward table, but only where needed.
2. **Store the id_key in the doc-table value** alongside the blob (value becomes
   `id_key_len + id_key + blob`) — every walk has the exact id_key, no 4th table,
   but a value-format change + a few bytes/doc.
3. **Accept a documented timeseries limitation** in step 1 (timeseries capped
   eviction / scan-delete unsupported), fix in a follow-up.
**Recommendation:** option 2 (store id_key in the value) — keeps the write count at
3, no extra table, and every scan trivially has the real (suffixed) id_key; costs a
small per-doc size + a value-format decode tweak. Decide with Joe before coding.

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

## Step 3 — capped-collection eviction + `$natural` hint on doc-table order.
## Step 4 — Python mirror (`src/secantus/storage.py`), byte-identical RecordId scheme.
## Step 5 — folded into step 1's migration if landable, else a dedicated pass.

## Measurement
Clean idle-machine A/B (load < 4; check for orphaned shells first —
[[orphaned-claude-shells-eat-cpu]]) at 1/2/4/8 writers, per step, vs the parent
commit. Expect +15% concurrency by the end.
