# RecordId keying — incremental implementation plan

**Goal:** cut write amplification from **4 WT writes/doc → 3** by keying the doc
table on a monotonic per-collection **RecordId** (the existing nat-seq) instead of
`id_key`. Measured earlier at **+15% concurrency** (prototype); the clean re-measure
(2026-07-23) confirms the concurrency gap is the dominant one, so this is the real
lever. **Highest-risk change in the codebase — a wrong `id_key→RecordId` hop is
silent data loss.** Build incrementally; each step is its own gated PR.

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
