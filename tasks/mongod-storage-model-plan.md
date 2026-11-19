# Plan: move the on-disk layout to mongod's storage model

Status: **open questions answered** (2026-11-19). No code has changed.

## Why

Joe, 2026-11-19: "lets move the storage model to mongodb's one. We shouldn't
diverge unnecessarily." Today's layout is a prototype choice, not a decision to
differ from mongod:

- The Python prototype (April 2026) kept every document in one shared
  `secantus_documents` table keyed by `(db, coll, id)`. The Rust storage ported
  it so both servers could share one on-disk format.
- PR #594 (2026-07-21) split that table into 16 hashed shards for writer
  concurrency (+19% at four writers).
- Per-collection tables were listed as the last phase of
  `tasks/rust-mongodb-parity-redesign.md`, funded only if the shared tables
  proved to be the limit. The oplog was the limit, so the phase never ran.

Measured 2026-11-19: per operation, the two layouts make the same number of
WiredTiger writes (insert 4, update 3, delete 5). So this is a fidelity change,
not a write-cost fix. It does make some operations structurally cheaper (drop
and rename, below).

## Decisions already made (Joe, 2026-11-19)

1. **Plan first.** This document is for review before any code.
2. **Refuse old stores.** A new build refuses a store in today's layout, with a
   message telling the user to dump and restore. No in-place migration.
3. **Both servers.** `secantusd-mdb` and `secantusd-pg` share
   `secantus-storage`, and both move. The SQL table read back as a collection
   keeps working.

## The target: mongod's model

To be confirmed in phase 0 by inspecting a real mongod 8.2.11 data directory
(`wt list` / `wt dump`). These are what is understood today, not yet measured.

| Part | mongod |
|---|---|
| Collection | One WT table per collection, named by an *ident* (`collection-<n>-<random>`). Key: RecordId (`q`). Value: the BSON document (`u`). |
| Index | One WT table per index (`index-<n>-<random>`). Key: KeyString of the indexed values, with the RecordId appended for a non-unique index. Value: KeyString TypeBits (and the RecordId for some unique-index formats). |
| `_id` | An ordinary unique index table, `_id_`. |
| Catalog | `_mdb_catalog`: a WT table keyed by RecordId. Each value is a BSON document holding the namespace, the collection options, the index specs and the ident of each table. |
| Sizes | `sizeStorer`: ident to `{numRecords, dataSize}`. |
| Oplog | The collection `local.oplog.rs`, keyed by a RecordId derived from the oplog timestamp. |
| Drop | WT `drop` of the table. A table still in use goes on a drop-pending list (the ident reaper) and is dropped once the timestamp it was dropped at is older than the oldest reader. |
| Rename | A catalog-only change: the ident and its file stay. |
| Logging | In a replica set, user tables are not logged; they rely on checkpoints and the oplog. The oplog and catalog are logged. |

## What changes, from today's layout

Inventory taken 2026-11-19. `S` means `crates/secantus-storage/src/lib.rs`.

| Today | Target | Main sites |
|---|---|---|
| 16 shared `secantus_documents_shN` tables keyed `(db, coll, RecordId)` (S:690-721) | One table per collection, keyed by RecordId | `doc_table_for` (54 hits), `frame_doc_value` (47) |
| Document value framed as `[len][id_key][BSON]` (S:1419) | Plain BSON | readers of `unframe_doc_value`; timeseries needs a decision (open question 3) |
| `_id` lookup through `secantus_natural_seq` `(db, coll, id_key) -> RecordId` (S:1643) | A real `_id_` index table | `NAT_SEQ_TABLE` (36), `find_by_id` (S:13968) |
| One shared `secantus_index_entries` table, key `(db, coll, index, entry)` (S:1619) | One table per index | `IDX_ENTRIES_TABLE` (33), `pack_entry` (S:1530) |
| Uniqueness in `secantus_unique_keys` (S:1630) | The unique index table itself | `UNIQ_TABLE` (41) |
| Index keys in our sort-key bytes (`secantus-core/src/sortkey.rs`) | KeyString | 88 `sortkey::` uses; explain, scan bounds and the planner read the bytes (open question 1) |
| Catalog tables `secantus_collections`, `secantus_indexes`, tombstones (S:680-687, S:1618) | `_mdb_catalog` with idents | `COLL_TABLE` (44), `IDX_TABLE` (30) |
| Chunked two-phase drop with tombstones (S:14597, S:14853) | WT drop plus a drop-pending ident reaper | drop and recovery code |
| Rename rewrites every row (S:15012) | Catalog-only rename | rename paths |
| `secantus_size_storer` keyed `(db, coll)` (sizes.rs:49) | `sizeStorer` keyed by ident | `sizes.rs` |
| 16 oplog shards keyed by seq (S:1672) | Keep for now (open question 2) | oplog code |
| Format markers: `entryFormat` 6, `numberKeys` 2, `idKeys` 2, upgrade-at-open paths (S:8430-8630) | One store-format version, and refusal of anything older | `upgrade_*`, `reject_legacy_*` |

What does not change: the WiredTiger version and its configuration, the
BSON-blob principle, the oplog-backed change streams, the per-collection
RecordId counter, and the PostgreSQL server's own catalog collections
(`__sql_catalog__` and the rest stay ordinary collections).

## Phases

Each phase lands as its own PR with the full storage gate (`cargo fmt`,
`clippy -D warnings`, `cargo test` from `crates/secantus-storage`), the
`rust-gate`, and the pymongo and psycopg gauges before merging.

0. **Measure the target.** Create a mongod 8.2.11 store (a one-member replica
   set, a collection, a compound index, a unique index, a TTL index, then drop
   and rename) and record its tables, key and value formats, catalog documents
   and size-storer rows. Check this document's target table against it.
   Output: a short appendix here, and the probe saved in `tools/probes/`.
1. **Store format version and refusal.** Add one store-format version, written
   when a store is created. A build refuses an older store by name, with the
   dump-and-restore instruction. All existing `upgrade_*` paths are removed.
   This lands with phase 2, not before, or `main` would refuse stores it
   still writes.
2. **Catalog and idents.** Add an `_mdb_catalog`-style table and ident
   allocation. Collection create, drop and rename go through it. Drops gain
   a drop-pending ident reaper. Collections still live in the shared tables
   until phase 3.
3. **One table per collection.** Documents move to their ident's table, keyed
   by RecordId, as plain BSON. Drop becomes a WT drop and rename a catalog
   change; the chunked purge and its tombstones go.
4. **One table per index, in KeyString.** Port mongod's KeyString encoding
   and TypeBits; each index gets its own table keyed by KeyString; `_id_`
   becomes a real unique index and `secantus_natural_seq` and
   `secantus_unique_keys` go. The 88 `sortkey::` sites that read stored bytes
   (explain, scan bounds, the planner) move to KeyString. In-memory
   comparisons in `secantus-core` that only order values stay as they are.
5. **Timeseries buckets.** Store time-series data as `system.buckets`
   bucket documents, as mongod does; remove the framed value.
6. **Size storer and the PostgreSQL server.** Key sizes by ident.
   `pg_relation_filepath` (secantus-pg/src/lib.rs:22636) returns the ident's
   file. Confirm the SQL-to-collection hand-off test still passes.
7. **mongod opens the store.** A test creates a store with SecantusDB,
   stops it, starts mongod 8.2.11 on the directory and reads every
   collection and index back.
8. **Clean-up.** Remove the legacy-table readers, the shard routing and the
   format markers this replaces; rewrite the layout tests below.

## Tests that pin today's layout

These change on purpose and are rewritten, not deleted:

- Byte-exact sort-key tests (`sortkey.rs`, 12 tests; `tests/test_sortkey.py`;
  `tests/test_rust_sortkey_parity.py` against the deprecated Python server,
  retired with a written reason).
- `entryFormat` tests (S:28101-28332, S:28848).
- `secantus-storage/tests/`: `compact.rs` and `repair.rs` list `secantus_*`
  tables; `drop_chunk.rs`, `rename_chunk.rs`, `natural_order.rs`,
  `size_storer.rs`, `unique.rs`, `indexes.rs`, `streaming_reads.rs`.
- `tests/test_rust_store_handoff.py` and `tests/test_number_key_format.py`.

New tests: a layout test per phase asserting the WT tables a store holds,
compared against the phase 0 mongod store; crash tests for drop and rename
mid-flight; the refusal message for an old store.

## Risks

- **Many WT tables.** A store with thousands of collections and indexes holds
  thousands of files and handles. mongod lives with this through WT's handle
  sweep and its session cursor cache; we will need the same settings and a
  test at, say, 10,000 collections.
- **Drop while in use.** WT refuses to drop a table with open cursors. The
  ident reaper must wait out readers without blocking the drop command, as
  mongod's does.
- **Startup.** Opening is per table, lazily, so startup should not grow, but
  measure it with `invoke startup-times`.
- **Concurrency.** Per-collection files are what PR #594 approximated with 16
  shards; the `do-perf` concurrency run must not regress.
- **The PostgreSQL server.** It shares the change. Coordinate with the session
  working the PG side before phases 3 and 6 land.
- **Backup and PITR archives** name tables; `pitr_archive.rs`, `repair.rs` and
  `compact.rs` need the new names.

## Decisions on the open questions (Joe, 2026-11-19)

1. **KeyString from the start.** The per-index tables are written in mongod's
   KeyString (with TypeBits) when they are created; there is no intermediate
   phase on today's sort-key bytes. Phases 4 and 5 merge.
2. **Oplog: keep the 16 seq-keyed shards for now.** Revisit after the
   KeyString work, with a `do-perf` concurrency run to decide whether one
   `local.oplog.rs` table keyed by timestamp holds up.
3. **Timeseries: follow mongod's buckets.** Time-series data is stored as
   bucket documents in a `system.buckets.<coll>` collection, as mongod does;
   the framed value goes entirely.
4. **Goal: files mongod can open.** A SecantusDB store must open in mongod
   8.2.11, and a test proves it. This means matching `_mdb_catalog`,
   `sizeStorer`, ident naming, the table configs (`app_metadata`, formats)
   and `WiredTiger.turtle` / `storage.bson` exactly, so phase 0's appendix
   becomes the specification, not a sanity check. Open sub-question for
   phase 0: the vendored WiredTiger is mongodb-7.0.33 while mongod 8.2 ships
   a newer WT; whether 8.2 opens files written by 7.0's WT must be measured
   first.

## Not in scope

- Changing WiredTiger's code or configuration (log size, spill engine).
- The Python servers, which are deprecated.
