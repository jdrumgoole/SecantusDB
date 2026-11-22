# Plan: move the on-disk layout to mongod's storage model

Status: **phase 0 done** (2026-11-19; see "Appendix: phase 0 results");
**phase 1 in review** (the WiredTiger build, snappy, `journal/`,
`storage.bson`, the format check). The release freeze starts when phase 1
lands: it changes the format, so no release of either Rust server is cut
until phase 6 has landed.

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
and rename, below). The probe behind those counts is not saved yet; phase 0
saves it beside its own.

## Decisions (Joe, 2026-11-19)

1. **Plan first.** This document is for review before any code.
2. **Refuse old stores.** A new build refuses a store in an older layout, with
   a message telling the user to dump and restore. No in-place migration.
3. **Both servers.** `secantusd-mdb` and `secantusd-pg` share
   `secantus-storage`, and both move. The SQL table read back as a collection
   keeps working.
4. **KeyString from the start.** The per-index tables are written in mongod's
   KeyString (with TypeBits) when they are created; there is no intermediate
   phase on today's sort-key bytes.
5. **Oplog: it moves to `local.oplog.rs` in this plan** (phase 4b). The
   first answer was to keep the 16 seq-keyed shards and revisit later;
   decision 8 replaced it, because a replica-set mongod needs the real
   oplog. The `do-perf` concurrency run is now phase 4b's gate, not a
   reason to defer.
6. **Timeseries: follow mongod's buckets.** Time-series data is stored as
   bucket documents in a `system.buckets.<coll>` collection, as mongod does;
   the framed value goes entirely.
7. **Goal: files mongod can open.** A SecantusDB store must open in mongod
   8.2.11, and a test proves it. Phase 0's appendix is therefore the
   specification, not a sanity check. What must match: `_mdb_catalog`,
   `sizeStorer`, ident naming, each table's creation config (`app_metadata`,
   key and value formats, block compressor, logging), `storage.bson`, and
   the collections a mongod expects in `local`, `admin` and `config`.
   `WiredTiger.turtle` and `WiredTiger.wt` are written by WiredTiger itself
   and are not matched by hand.
8. **mongod starts on our store as a replica-set member.** The phase 7 test
   starts mongod 8.2.11 with `--replSet`, not standalone. So the store holds
   a real `local.oplog.rs` keyed by timestamp, `local.system.replset` and the
   `local.replset.*` collections, and follows mongod's replica-set logging
   model (user tables not logged; recovery from the stable checkpoint plus
   the oplog).
9. **If mongod 8.2 cannot open files the vendored WiredTiger wrote, the
   vendored WiredTiger is upgraded** to the version mongod 8.2.11 ships
   (phase 0d). The goal in decision 7 is not dropped.
10. **The reverse direction is a goal, in this plan.** SecantusDB opens and
    serves a data directory mongod 8.2.11 wrote. Phase 7 tests both
    directions. Anything in such a store that SecantusDB cannot serve is
    refused by name when the store is opened, never skipped.

## The target: mongod's model

Measured in phase 0 on mongod 8.2.11 (FCV 8.2, one-member replica set); the
full record is `tests/fixtures/storage_model/mongod-8.2.11-layout.json`,
written by `tools/probes/mongod_store_layout.py`.

| Part | mongod 8.2.11 |
|---|---|
| Collection | One WT table per collection, named by an *ident* `collection-<UUID>` (a random UUID, not `<n>-<random>`). Key: RecordId, `key_format=q`. Value: the BSON document, `value_format=u`. |
| Clustered collection | Same, but `key_format=u`: the RecordId is the KeyString of `_id` (no type bits, no end byte). No `_id_` index table. `system.buckets.*` and `config.system.preimages` are clustered. |
| Index | One WT table per index, `index-<UUID>`, `key_format=u,value_format=u`. A non-unique index (`app_metadata=(formatVersion=8)`) keys on the KeyString with the RecordId appended; the value is the TypeBits, empty when there are none. A unique secondary index (`formatVersion=14`) has the same key layout, RecordId appended. |
| `_id` | Its own table (`formatVersion=8`) in the older unique layout: the key is the KeyString of `_id` alone, and the value is the RecordId followed by the TypeBits. |
| Catalog | `_mdb_catalog`, `key_format=q`. Each value is `{md: {ns, options, indexes: [{spec, ready, multikey, multikeyPaths}]}, idxIdent: {<index name>: <ident>}, ident, ns}`. `options` holds the collection `uuid` and options such as `capped`, `size`, `max`, `clusteredIndex`, `validator`, `collation`. |
| Sizes | `sizeStorer`, `key_format=u`: the key is `table:<ident>`, the value `{numRecords, dataSize}`. `_mdb_catalog` has a row of its own. Logged. |
| Oplog | `local.oplog.rs`: capped (`size` 192 MB on this box), `autoIndexId: false`, no indexes, `app_metadata=(formatVersion=1,oplogKeyExtractionVersion=1)`, logged. |
| Drop | The dropped collection's tables stay on disk (drop-pending) until the drop timestamp is older than the checkpoint; its catalog entry goes at once. |
| Rename | Catalog-only: `app.old` renamed to `app.renamed` kept its idents. |
| Logging | Replicated user and catalog tables are `log=(enabled=false)`, `_mdb_catalog` included. Logged: `local.oplog.rs`, `sizeStorer`, and the unreplicated `local.*` collections except `local.replset.minvalid`. The connection's log lives in `journal/`, snappy-compressed. |
| Table configs | Collections: `block_compressor=snappy` (`zstd` for time-series buckets), `prefix_compression=false`, `leaf_page_max=32KB`, `leaf_value_max=64MB`, `split_pct=90`, `app_metadata=(formatVersion=1)`. Indexes: no block compressor, `prefix_compression=true`, `internal_page_max=16k`, `leaf_page_max=16k`. |
| Files | `storage.bson` = `{storage: {engine: "wiredTiger", options: {directoryPerDB: false, directoryForIndexes: false, groupCollections: false}}}`. WiredTiger 12.0.0 writes the turtle. |
| Server collections | `local`: `oplog.rs`, `startup_log`, `system.replset`, `system.rollback.id`, `replset.{minvalid, election, oplogTruncateAfterPoint, initialSyncId}`. `admin`: `system.version` (FCV), `system.keys`, `system.users`, `system.roles`. `config`: `transactions`, `image_collection`, `system.indexBuilds`, `system.preimages`, `analyzeShardKeySplitPoints`, `sampledQueries`, `sampledQueriesDiff`. |
| Views | A `<db>.system.views` collection holding the view definitions. |
| `internal-<UUID>` | Two drop-pending temporary tables left by index builds (`key_format=q`). |

## What changes, from today's layout

Inventory taken 2026-11-19. `S` means `crates/secantus-storage/src/lib.rs`;
hit counts are word matches across `crates/secantus-storage/src`.

| Today | Target | Main sites |
|---|---|---|
| 16 shared `secantus_documents_shN` tables keyed `(db, coll, RecordId)` (S:690-721) | One table per collection, keyed by RecordId | `doc_table_for` (54 hits), `frame_doc_value` (20) |
| Document value framed as `[len][id_key][BSON]` (S:1419) | Plain BSON | readers of `unframe_doc_value` (S:1448); timeseries moves to buckets (decision 6) |
| `_id` lookup through `secantus_natural_seq` `(db, coll, id_key) -> RecordId` (S:1643) | A real `_id_` index table | `NAT_SEQ_TABLE` (36), `find_by_id` (S:13968) |
| One shared `secantus_index_entries` table, key `(db, coll, index, entry)` (S:1619) | One table per index | `IDX_ENTRIES_TABLE` (33), `pack_entry` (S:3545) |
| Uniqueness in `secantus_unique_keys` (S:1630) | The unique index table itself | `UNIQ_TABLE` (40) |
| Index keys in our sort-key bytes (`secantus-core/src/sortkey.rs`) | KeyString (decision 4) | 89 `sortkey::` uses across `crates/`: 44 in S, 12 in `secantus-commands/src/explain/stream.rs`, 4 in `scan_bounds.rs`, the rest in `secantus-core` |
| Catalog tables `secantus_collections`, `secantus_indexes`, tombstones (S:680-687, S:1618) | `_mdb_catalog` with idents | `COLL_TABLE` (29), `IDX_TABLE` (30) |
| Chunked two-phase drop with tombstones (S:14597, S:14853) | WT drop plus a drop-pending ident reaper | drop and recovery code |
| Rename rewrites every row (S:15012) | Catalog-only rename | rename paths |
| `secantus_size_storer` keyed `(db, coll)` (sizes.rs:49) | `sizeStorer` keyed by ident | `sizes.rs` |
| 16 oplog shards keyed by seq (S:1672), `secantus_oplog_meta` (S:2108) | `local.oplog.rs`, keyed by a RecordId derived from the timestamp (decision 5, phase 4b) | oplog code, change streams, PITR |
| Tables `block_compressor=lz4` (S:2722, S:2726) | snappy for collections, zstd for buckets, none for indexes; built in by default (`SECANTUS_WT_EXTRA_COMPRESSORS` is off today) | `DOC_TABLE_CFG`, `QU_COMPRESSED_CFG`, `CMakeLists.txt`; needs a `do-perf` run |
| WiredTiger built with `WT_STANDALONE_BUILD=ON`, which stamps btree files version 2.1 | Built `WT_STANDALONE_BUILD=OFF`, as mongod builds it (version 1.1 files) | the three WT builds; see the appendix |
| Most tables already `log=(enabled=false)`; logged: the oplog tables, `secantus_oplog_meta`, `secantus_size_storer`, `secantus_index_builds` (measured on 0.5.3-beta.198) | The mongod set listed in the target table (decision 8) | table-create configs, recovery |
| WiredTiger log in the store's top directory | `journal/`, snappy-compressed | connection config (S:2673) |
| Format markers: `entryFormat` 6, `numberKeys` 2, `idKeys` 2, upgrade-at-open paths (S:8430-8630, `upgrade_number_keys` S:19130, `upgrade_id_keys` S:19309) | One store-format version, and refusal of anything older | `upgrade_*`, `reject_legacy_*` |

Tables the first draft missed. Phase 0 decides, for each, between "becomes
the mongod collection named" and "stays a private table mongod ignores";
the default is the mongod collection wherever one exists.

| Today | mongod's home for it |
|---|---|
| `secantus_users`, `secantus_roles`, `secantus_role_seq` (S:2114-2121) | `admin.system.users`, `admin.system.roles` |
| `secantus_preimages` (S:2102; a comment at S:27853 says builds since RM-366 no longer write it, so confirm what holds pre-images now) | `config.system.preimages` |
| `secantus_prepared_xacts` (S:2779) | mongod keeps prepared transactions in the oplog and `config.transactions`; PostgreSQL two-phase commit has no mongod home, so this likely stays private |
| `secantus_index_builds`, `secantus_index_build_side` (index_build.rs:41-44) | mongod's index-build state lives in the catalog entry (`ready: false`) plus internal side-write tables (`internal-<n>-<random>`) |
| `secantus_profile_settings` (S:2122) | none on disk (the profile level is per-process in mongod); likely stays private |

What does not change: the BSON-blob principle, change streams being
oplog-backed (the oplog's own table does change), the per-collection RecordId counter, and the PostgreSQL server's own
catalog collections (`__sql_catalog__` and the rest stay ordinary
collections). The vendored WiredTiger version changes only if phase 0a
finds mongod cannot open its files (decision 9). Table creation configs **do** change (compressor,
logging, `app_metadata`), because decision 7 requires it.

## Phases

### Gate for every phase

Each phase lands as its own PR, after all of:

- the storage gate (`cargo fmt`, `clippy -D warnings`, `cargo test`) run from
  `crates/secantus-storage` and from each other WiredTiger-linked crate's own
  directory, and `./inv rust-gate`;
- `invoke rust-stress` and the crash tests (drop, rename and recovery all
  change);
- the explain sweeps `tools/probes/explain_commands.py` and
  `tools/probes/find_order.py` (explain and scan bounds read key bytes);
- `tests/mongod_live/` and the SQL-to-collection hand-off test
  (`tests/test_rust_pgserver_slice.py`);
- the gauges: pymongo for every phase; psycopg, sqllogictest and SQLAlchemy
  for any phase that touches index keys or the PG server;
- `do-perf` concurrency and `invoke startup-times`, compared against the
  commit before the phase, on a pinned worktree;
- from phase 2 on, the phase 0 comparison tool: the same operations run on
  mongod and on SecantusDB, then `wt list` and the catalog documents diffed.
  Each phase states which differences are still expected.

### Store-format version across phases

Confirmed by Joe, 2026-11-19.

Phases 2 to 5, 4b included, each change the format. Each one **bumps the store-format
version**, so a store written by an earlier phase is refused by name like any
other old store. **No release of either Rust server is cut between phase 2
landing and phase 6 landing**, so no user ever holds an intermediate format.

### The phases

0. **Measure the target.**
   - **0a. WiredTiger compatibility, first.** Write a store with the vendored
     WT 7.0.33 holding mongod-shaped tables, and start mongod 8.2.11 on it.
     Record which compressors mongod's WT accepts, what FCV document it
     demands, and whether it then rewrites files so that WT 7.0.33 can no
     longer open them. If mongod refuses the files, phase 0d runs before
     phase 1.
   - **0b. The specification.** Create a mongod 8.2.11 store, started as
     a one-member replica set: a collection, a compound index, a unique index,
     a TTL index, one index of each kind listed under phase 4, a capped
     collection, a clustered collection, a time-series collection, a user and
     a role, a collection with pre-images, then a drop and a rename. Record
     its tables, their creation configs, key and value formats, catalog
     documents, size-storer rows, `storage.bson`, and every collection in
     `local`, `admin` and `config`.
   - **0c. The comparison tool.** A probe that runs one script of operations
     against mongod and against SecantusDB and diffs `wt list`, the table
     configs and the catalog documents.
   - **0d. WiredTiger upgrade, only if 0a requires it.** Move
     `vendor/wiredtiger` to the version mongod 8.2.11 ships. All three
     builds apply the same patch list; every `cmake/patch_wt_*.py` patcher
     is re-proved against the new source, the two that change group commit
     under `dsync` / `fsync` with a durability test; the PGO profile is
     refreshed. It lands as its own PR, with the full per-phase gate and
     every driver gauge, before phase 1.
   - Output: an appendix here, the probes saved in `tools/probes/`, and the
     logging decision recorded in the "what changes" table. **Done**: see
     the appendix.
1. **Store format version and refusal.** Add one store-format version, written
   when a store is created. A build refuses an older store by name. The
   message names the last release that reads the old layout and gives the
   dump-and-restore commands for each server (`mongodump` / `mongorestore`;
   `pg_dump` / `psql`). All existing `upgrade_*` paths are removed. This
   lands with phase 2, not before, or `main` would refuse stores it still
   writes.

   **Landed alone, ahead of phase 2** (2026-11-19): phase 1 also changes
   the format (below), so it refuses only stores it cannot read, and the
   freeze starts with it. The `upgrade_*` and `reject_legacy_*` paths stay
   until phase 8: no store that needs them can reach them any more.
   The format version lives in a `secantus.format` file beside
   `storage.bson` (mongod ignores it); a store with `storage.bson` and no
   such file is mongod's, refused until phase 7's reverse direction.

   The same change switches the three WiredTiger builds to
   `WT_STANDALONE_BUILD=OFF` with snappy and zstd built in (phase 0a). That
   alone makes every existing store unreadable (WiredTiger refuses version
   2.1 files with `EACCES`), so the format check must run **before**
   `wiredtiger_open` and recognise an old store by its files, never by
   letting the open fail.

   Before it lands: prove `pg_dump` from `secantusd-pg` and a restore into
   the new build work end to end on a store with real content (open
   `pg_dump` items are in `tasks/backlog-rust-postgres.md`), and the same
   for `mongodump`. A refusal with no working exit is data loss.
2. **Catalog and idents.** Add `_mdb_catalog` and ident allocation, matching
   the phase 0 catalog documents. Collection create, drop and rename go
   through it. Collections still live in the shared tables until phase 3.
3. **One table per collection.** Documents move to their ident's table, keyed
   by RecordId, as plain BSON, with mongod's table config. Drop becomes a WT
   drop with a drop-pending ident reaper; rename becomes a catalog change;
   the chunked purge and its tombstones go. Clustered collections
   (byte-string RecordId) land here, since phase 5 needs them. The tables in
   the "missed" inventory that have a mongod home become those collections
   here.
   **Phases 2 and 3 land together** (2026-11-19): a catalog row naming an
   ident whose table does not exist yet would be a third intermediate format
   for nothing. As built:
   - `_mdb_catalog` is written beside this server's own registry
     (`secantus_collections`, `secantus_indexes`), in the same WiredTiger
     transaction, at every registry write (`sync_catalog_entry`), and
     rewritten whole at each open. The registry stays the working set its
     transactional machinery (deferred creates, catalog generations) is
     built on; the catalog is what mongod reads, and phase 7's reverse
     direction rebuilds the registry from it.
   - Where a collection's documents live is `secantus_idents`, `(db, coll)
     -> {ident, catalogId}`, so a lookup is one search in the reader's
     snapshot; a collection created inside a transaction keeps its
     placement on the transaction until commit, as its registry row is.
   - Documents keep the framed value (`[len][id_key][BSON]`) until phase 5,
     which needs it gone for buckets anyway; the `_id_` and secondary index
     rows stay in the shared tables, keyed by namespace, until phase 4.
     A rename therefore still rewrites those rows in chunks, but moves the
     documents' table whole; a drop unlists the table and drops it once its
     transaction commits and nothing has it open, and an open drops any
     `collection-*` table no placement names.
   - Not yet, and where it goes: clustered collections need KeyString
     RecordIds (phase 4); views are kept in the registry, not in a
     `system.views` collection (phase 7, which must read mongod's); a read
     in a snapshot older than a drop finds the table gone and is refused
     with mongod's `SnapshotUnavailable` message, but the reaper does not
     yet wait for the oldest reader before dropping, as mongod's does --
     phase 4b, when the store keeps mongod's timestamps.
   - Store format version 2.
4. **One table per index, in KeyString.** Port mongod's KeyString encoding
   and TypeBits; each index gets its own table keyed by KeyString; `_id_`
   becomes a real unique index and `secantus_natural_seq` and
   `secantus_unique_keys` go. The `sortkey::` sites that read stored bytes
   (explain, scan bounds, the planner) move to KeyString. In-memory
   comparisons in `secantus-core` that only order values stay as they are.

   mongod must be able to read every index we write, so each index kind
   needs mongod's exact keys. For each, phase 0b records the bytes and this
   phase either matches them or refuses to create the index by name, with a
   backlog item:

   | Index kind | What must match |
   |---|---|
   | Plain, compound, multikey, unique, partial, sparse, TTL | KeyString and TypeBits; multikey paths in the catalog entry |
   | Collated | ICU sort keys, from the ICU version mongod 8.2.11 links |
   | `hashed` | mongod's hash function |
   | `2dsphere` | the S2 cell covering and index version |
   | `2d` | the geohash key |
   | Text | the FTS key format and text index version |
   | Wildcard | the `$_path` key layout (not present in S today; confirm whether the server supports it at all) |

   **PostgreSQL server.** Its index order is stored in the same key bytes,
   so this phase also covers how PG orderings are kept in KeyString: NULL
   placement (`NULLS FIRST` / `LAST`), `numeric`, `text` under a collation,
   descending keys, and the B-tree and hash page models built over them.
   Each is probed against PostgreSQL 15 before and after.
   **Phase 4 design (2026-11-19).** Done first: `secantus_core::keystring`,
   a port of mongo r8.2.11's `key_string.cpp` (V1, TypeBits, RecordIds,
   decoding), which reproduces every key and TypeBits row of the phase 0
   fixture and 280 hard numbers recorded from mongod
   (`tools/probes/mongod_keystring_corpus.py`), and orders as
   `secantus_core::order` does. The storage work, in this order, each step
   with the full storage suite green:
   - **4a. Index idents and tables.** Each index, `_id_` included, gets an
     ident `index-<UUID>` and a table made with mongod's index config
     (`key_format=u,value_format=u`, prefix compression, 16k pages,
     `app_metadata=(formatVersion=8)`, `14` for a unique secondary index).
     The idents live in the collection's placement (`secantus_idents`) and
     in `_mdb_catalog`'s `idxIdent`; a rollback's or crash's orphan
     `index-*` table is dropped as a collection's is.
   - **4b. Secondary entries in KeyString.** An entry is the KeyString of
     the index key's values (directions from the key pattern, strings
     through the index's collation sort key) followed by the RecordId; its
     value is the TypeBits. The key generation stays (multikey, sparse,
     partial, the existing value expansion): only the bytes change. Replaces
     `secantus_index_entries`.
     **As built (4a and 4b, store format 3):**
     - Only secondary indexes have a table; `_id_` has none until 4d. The
       idents are in the placement (`secantus_idents`' `indexes`, name to
       ident) and in `_mdb_catalog`'s `idxIdent`. A build makes the table
       first and lists it in the placement in the transaction that writes the
       registry row, so a crash or an abort leaves an unlisted table, which
       the next open drops; a rollback drops it through the transaction's end
       hook (a deferred create keeps it in the transaction's own placement).
       A drop or rename moves no entries: the table follows the placement.
     - The reaper drops with `force`, as mongod's ident drop does
       (`force,checkpoint_wait=false`): without it, a pooled session's
       cached data handle kept a table from ever being dropped (`EBUSY`),
       which also meant phase 3's dropped collection tables went only at the
       next open. A table with a cursor open on it still refuses and waits.
     - A dropped table goes only once a checkpoint begun after its drop has
       completed (`DropPending`), as mongod keeps a drop-pending ident until
       the checkpoint no longer needs it: until then a read of the last
       checkpoint (a background `validate`) still finds the collection.
       Dropping at once lost that read about one time in thirteen under load
       (`diagnostic_commands_wt::background_validate_reads_the_last_checkpoint`).
     - A collection created by a transaction that does not defer its creates
       (the PostgreSQL server's) and then rolled back no longer leaves its
       tables: `ensure_collection` lists them for the drop-pending reaper
       when the transaction ends without committing (4d).
     - Key generation is unchanged, so an array still gets the whole-array
       entry besides mongod's element keys (`secantus_core::index_keys`).
       mongod's `validate` would count those as extra keys: phase 7 must
       drop them or refuse the index.
     - A geo index's cell is keyed as one BinData value, not mongod's key.
       The PostgreSQL server's `bytea` order (shorter first) is kept by keying
       a BinData value of such an index as text that orders so; the decode
       of such an entry gives the text.
     - The bounds work of 4c is done here for secondary indexes
       (`scan_bounds::to_key_spans`, `Storage::index_entries_from` taking an
       `IndexPos` and returning each entry's decoded `raw` and a `resume`
       key); the explain stream's own comparisons still run on sort-key
       parts built from the decoded values. `_id_` keeps its sort-key bytes
       until 4d.
   - **4c. Scans and bounds.** `scan_bounds` builds KeyString bounds with
     mongod's discriminators (`ExclusiveBefore` / `ExclusiveAfter`) in place
     of sort-key prefixes; the explain stream decodes stored keys with
     `keystring::to_bson` instead of splitting sort-key parts. In-memory
     comparisons (sorting, grouping) keep `sortkey`: they never reach disk.
   - **4d. `_id_` as a real unique index.** Key = KeyString of `_id`; value
     = RecordId + TypeBits, as mongod's `_id_` stores it. Replaces
     `secantus_natural_seq`; `find_by_id` and the RecordId lookup read it.
   - **4e. Uniqueness in the index tables.** A unique secondary index checks
     for an entry with the same KeyString prefix and another RecordId, as
     mongod's does; `_id_` checks its key. Replaces `secantus_unique_keys`,
     its claims (`prepareUnique`, collated `_id`, deferred-create
     incarnations) moving to the index tables.
   - **4f. Clean-up.** The number-key, id-key and entry-format upgrade and
     refusal paths go (no store that needs them opens); store format 3.
     **As built (4d, 4e and 4f, store format 4):**
     - `_id_` is an `index-<UUID>` table made with the collection
       (`INDEX_TABLE_CFG`, `formatVersion=8`), listed in the placement and in
       `idxIdent`. Key = KeyString of `_id` alone, value = the RecordId
       (`append_record_id_long`'s bytes) then the TypeBits; the tables of the
       phase 0 fixture (`app.c`, `app.ks`, `app.system.views`) are rebuilt
       byte for byte (`tests/id_index_table.rs`). A collection's default
       collation is the key's string transform. `system.profile` and a
       time-series collection have no `_id_` (a measurement repeats `_id`; a
       profile entry has none); a clustered collection (still not mongod's:
       its RecordId is not the `_id`) keeps an `_id_` table.
     - `idIndexSize` counts the `_id_` entries' key bytes
       (`Delta::entries("_id_", ..)`); `Delta::insert/remove/replace` no
       longer carry a key length.
     - Uniqueness (4e): a unique or `prepareUnique` secondary index claims the
       KeyString of the value alone (insert with `overwrite=false`, remove
       in the same transaction: mongod's `WiredTigerIndex::_checkDups`
       phase 1) and then looks for another RecordId's entry under that
       prefix (phase 2); `_id_` is the plain `overwrite=false` insert of
       `WiredTigerIdIndex::_insert`. `secantus_unique_keys` and its
       take-over of stale claims, `deferred_keys` and `move_keyed_rows`, the
       collated and document `_id` claims and their backfills, and the
       rename's copy are gone.
     - Cleanup (4f): the number-key, `_id`-key and entry-format markers and
       their upgrade, re-key, refusal and rebuild paths, `NAT_SEQ_TABLE`,
       `UNIQ_TABLE`, `to_spans` and `widen_ranks` (an `_id` range is
       `to_key_spans` over the `_id_` table), `IndexPos::Raw` /
       `IndexFrom::Raw`; the explain stream reads `_id_` as any other
       index. `migrate_legacy_pre_images` (a pre-RM-366 table) stays: not
       one of the formats listed.
   - **The PostgreSQL server** reaches indexes through the same storage
     calls; its NULL placement and types are checked by the psycopg, slt and
     SQLAlchemy gauges after 4b and after 4e.
4b. **The oplog and replica-set state.** The oplog becomes the collection
   `local.oplog.rs`, keyed by a RecordId derived from the timestamp, with
   entries in mongod's oplog format; the 16 shards and `secantus_oplog_meta`
   go. Add `local.system.replset`, the `local.replset.*` collections and
   whatever else phase 0b found a one-member replica set keeps. User tables
   move to mongod's logging model: not logged, with a stable timestamp, and
   recovery that replays the oplog from the stable checkpoint. Change
   streams, resume tokens, pre-images, PITR and the PostgreSQL server's
   commit path all read or write the oplog and move with it. This is the
   phase that PR #594's shards were built to avoid, so its gate is the
   `do-perf` concurrency run at 1, 4 and 16 writers; a regression there is
   fixed in this phase, not accepted.

   **Phase 4b design (2026-11-20).** Measured with
   `tools/probes/mongod_oplog_corpus.py` on a one-member mongod 8.2.11
   replica set (`tests/fixtures/storage_model/mongod-8.2.11-oplog.json`, 19
   operations, 42 stored rows):

   - **The key is the timestamp.** Every row's RecordId is `(secs << 32) |
     inc` of its `ts` (42 of 42). The table is `key_format=q`, snappy,
     logged, `app_metadata=(formatVersion=1,oplogKeyExtractionVersion=1)`;
     the catalog entry's options are `{uuid, capped: true, size: 201326592,
     autoIndexId: false}`, with no indexes. `find` returns the stored
     documents unchanged: mongod has no view over its oplog.
   - **One entry per mongod entry.** A single write, and each document of an
     `updateMany`, is its own entry. An `insertMany` and a `deleteMany` are
     one `applyOps` at one timestamp; so is a transaction. A retryable
     batched insert carries `lsid`, `txnNumber`, `prevOpTime` and
     `multiOpType: 1`; a non-retryable `deleteMany` carries none of them.
   - **Field order:** `lsid, txnNumber, op, ns, ui, o, o2, needsRetryImage,
     stmtId, ts, t, v, wall, prevOpTime` (an `applyOps` element: `op, ns,
     ui, o, o2, stmtId`). A `findAndModify` stores `needsRetryImage:
     "preImage"` and keeps its image in `config.image_collection`.
   - **DDL:** `create` carries `idIndex`. `createIndexes` is a two-phase
     build: `config.system.indexBuilds` insert, `startIndexBuild`, an update,
     `commitIndexBuild`, a delete. `collMod` has `o2.collectionOptions_old`;
     `dropIndexes` has `o2` = the dropped spec; `drop` has
     `o2.numRecords`; `dropDatabase` is a `drop` per collection, then
     `dropDatabase`.

   **Measured 2026-11-20 (batching, `partialTxn`, prepare; mongod 8.2.11,
   one-member set).** `tools/probes/mongod_oplog_batching.py` and
   `tools/probes/mongod_oplog_prepare.py` write
   `tests/fixtures/storage_model/mongod-8.2.11-oplog-batching.json` and
   `...-oplog-prepare.json`:

   - **Insert.** `insertMany` (ordered or not, retryable or not) is ONE
     `applyOps` entry at one ts, whatever the count (200 documents: one
     entry, 18,795 bytes); a one-document `insertMany` is a plain insert
     entry. It is cut by size: five 1 MB documents are five plain entries.
     `bulkWrite` is one entry per operation.
   - **Delete.** `deleteMany` is `applyOps` of deletes, in batches of ten
     (200 matches: 20 entries at consecutive ts, each ten elements); one
     match is a plain delete entry; five 1 MB documents split 3 + 2. A
     retryable `deleteMany` carries no `lsid`/`txnNumber` (the command is not
     retryable); a retryable `insertMany` has `lsid`, `txnNumber`,
     `prevOpTime: {ts: Timestamp(0,0), t: -1}` and `multiOpType: 1`, and its
     elements carry `stmtId`. `updateMany` is one plain entry per document
     (no `applyOps`), even in a retryable session. An `applyOps` element is
     `op, ns, ui, o, o2` (insert; `o2` = `{_id}`), `op, ns, ui, o` (delete),
     with `stmtId` last when retryable.
   - **Transaction.** One `applyOps` entry at the commit ts, fields `lsid,
     txnNumber, op:"c", ns:"admin.$cmd", o:{applyOps}, ts, t, v, wall,
     prevOpTime` (no `multiOpType`); inserts, deletes and updates of a
     transaction (11 ops across `insertMany`, `deleteMany`, `updateMany`) are
     all elements of the one entry. An aborted or empty transaction writes
     nothing.
   - **`partialTxn` chain.** Twenty 1 MB inserts: two entries at consecutive
     ts (`t.1`, `t.2`). The first is `o: {applyOps: [16 ops], partialTxn:
     true}`, 16,001,861 bytes, `prevOpTime` `{Timestamp(0,0), -1}`; the last
     is `o: {applyOps: [4 ops], count: 20}`, 4,000,657 bytes, its
     `prevOpTime` the first entry's `{ts, t}`. An entry takes whole
     operations while it stays under about 16 MB (BSON's internal limit
     16,793,600).
   - **Prepare** (needs `--shardsvr` and a `shardIdentity`, which the probe
     builds; a plain replica set answers `prepareTransaction` with "Cannot
     accept sharding commands"). `prepareTransaction` writes the `applyOps`
     entry with `o: {applyOps, prepare: true}` at the PREPARE ts (the
     transaction's operations all in it); `commitTransaction` writes
     `{op:"c", ns:"admin.$cmd", o:{commitTransaction: 1, commitTimestamp:
     <prepare ts>}, prevOpTime: <the prepare entry's {ts, t}>}` at a later
     ts; `abortTransaction` writes `o:{abortTransaction: 1}` with the same
     `prevOpTime`. Both carry `lsid` and `txnNumber` (the probe strips `wall`, which
     every entry has).
   - **Not yet measured:** the two-phase index build entries beyond the
     existing fixture; a prepared transaction over 16 MB; recovery from a
     crash between prepare and commit.

   Today's store differs in all of these: one row per operation, each at its
   own timestamp, keyed by a seq over two to sixteen shards, with private
   fields (`rw`, `bg`, `pi`) and a view (`oplog_view_entries`) that collapses
   rows into `applyOps` for `find`. The steps:

   - **4b-1. The table.** `local.oplog.rs` is a capped collection in its own
     `collection-<UUID>` table, with the measured config and catalog entry,
     created with the store. RecordId = timestamp. The shards, the legacy
     `secantus_oplog` table and `secantus_oplog_meta`'s `state` row go: at
     open the next timestamp follows the last entry, as mongod reads the top
     of its oplog. The asynchronous drainer (`SECANTUS_OPLOG_ASYNC`) goes:
     mongod writes the oplog in the writer's transaction, and the drainer
     wrote it after.
   - **4b-2. Entries in mongod's form.** What is stored is what `find`
     returns, in the measured field order. A batched insert or delete, and a
     transaction, store one `applyOps` at the commit timestamp; one over
     mongod's size limit becomes mongod's `partialTxn` chain linked by
     `prevOpTime` (measure the limit and the chain). A transaction's
     operations collect on its handle until commit, as mongod's
     `TransactionOperations` do, so the PostgreSQL server's write set and
     READ COMMITTED overlay (`transaction_write_set`, `block_overlay`) read
     the handle rather than uncommitted rows. A prepared transaction
     (PostgreSQL's two-phase commit) writes mongod's prepare form, an
     `applyOps` with `prepare: true` and then `commitTransaction` or
     `abortTransaction` (measure them). The private fields go: `rw` becomes
     `lsid`/`txnNumber`/`stmtId`/`needsRetryImage` with
     `config.image_collection`; `bg` becomes the `applyOps`; `pi` is
     already `config.system.preimages`' `{nsUUID, ts, applyOpsIndex}`. DDL
     entries take the measured shapes, index builds included.
   - **4b-3. Positions are timestamps.** Every reader moves from seq to `ts`,
     or `(ts, applyOpsIndex)` inside an `applyOps`: change streams (which
     unwind `applyOps` with `txnOpIndex`, as mongod's do), `awaitData`,
     retryable-write history, pre-images, replay and restore, the PITR
     archive (segments name a first and last timestamp and chain by
     abutting, not by seq arithmetic), sizes and `lastOp`. `find` and
     tailable cursors on `local.oplog.rs` become ordinary scans of a capped
     collection, which also gives a tailable cursor there new entries (it
     sees none today). Oplog readers still see only up to the point with
     no uncommitted entry below it (mongod reads WiredTiger's
     `all_durable`); the in-flight floor that computes it is kept, keyed by
     timestamp, because the PostgreSQL store is not timestamped.
   - **4b-4. Resume tokens in mongod's form.** `_data` becomes mongod's
     KeyString of `{clusterTime, version, tokenType, txnOpIndex,
     fromInvalidate, uuid, eventIdentifier}`, built with
     `secantus_core::keystring`, so mongod resumes from our token (phase 7)
     and we resume from mongod's. Measured per event kind against mongod.
     `resume_keystring.rs` already decodes mongod's tokens.
   - **4b-5. Truncation as mongod's.** The oplog is held to its `size` in
     bytes (mongod's default rule; 192 MB on this box), truncated from the
     oldest end in whole entries, never past the stable checkpoint's
     timestamp, and `replSetResizeOplog` changes it. The entry-count cap
     (`max_entries`, 100,000) goes.
   - **4b-6. Replica-set state and recovery.** `secantus_oplog_meta` goes.
     The store keeps what phase 0b found a one-member set keeps:
     `local.replset.minvalid` (not logged), `local.replset.oplogTruncateAfterPoint`,
     `local.replset.election`, `local.replset.initialSyncId`,
     `local.system.rollback.id` and `local.system.replset`, plus
     `config.system.indexBuilds` for 4b-2's index builds. The recovery point is WiredTiger's checkpoint
     timestamp (`get=recovery`, as now); recovery truncates the oplog after
     `oplogTruncateAfterPoint` and replays what follows the recovery point,
     as mongod's `ReplicationRecovery` does.
   - **The PostgreSQL store keeps logged, untimestamped tables for now.**
     Its durability model is PostgreSQL's, and nothing in 4b needs it to
     change. Whether mongod can open such a store as a replica-set member
     is measured in phase 7 (RP-1074), and a change to it is a decision for
     Joe then.

   **As built (4b-1 and 4b-3, store format 5; entries still one row per
   operation, so 4b-2 is the next stage):**
   - `local.oplog.rs` is a `collection-<UUID>` table made with mongod's
     config (`COLLECTION_TABLE_CFG` with `oplogKeyExtractionVersion=1`, snappy,
     logged, `split_pct=90`: measured, not the old shard tuning) and an
     `_mdb_catalog` row `{md: {ns, options: {uuid, capped, size, autoIndexId},
     indexes: []}, idxIdent: {}, ident, ns}`. The registry and `secantus_idents`
     do not list it. **The catalog row alone cannot find it**: `_mdb_catalog`
     is not logged in a store whose data tables are not (the server's mode), so
     after a kill -9 before the first checkpoint the row is gone while the
     logged oplog holds every acknowledged write; an open that trusted the
     catalog made a new empty oplog and dropped the real one as an orphan
     (`tests/test_crash_recovery.py` and `test_rust_binary_crash_durability.py`
     caught it; the kill harness now kills a store's first life too). The ident
     and uuid are therefore recorded in the always-logged oplog meta table
     (`oplog` row), the catalog row is derived from it (written again at open
     if absent), and `reconcile_idents` keeps the recorded ident.
     `Storage::oplog_table` is its URI. The
     sixteen shards, the legacy `secantus_oplog`, the `state` row of
     `secantus_oplog_meta` and the asynchronous drainer (`--oplog-async`,
     `SECANTUS_OPLOG_ASYNC`, `StorageOptions::oplog_async`) are gone.
   - **A position is a timestamp.** The RecordId of a row is its `ts`
     (`secs << 32 | inc`), and every reader that took a seq now takes that
     position as an `i64`: change-stream resume tokens (`s` is the position),
     `awaitData`, `find_seq_for_ts`, pre-images, retryable history, replay,
     `import_oplog_segment`, the stable marker, the write set of a prepared
     transaction. A run of entries takes consecutive timestamps (`mint_run`,
     as mongod's `reserveOpTimes`), so `start + i` still names the i-th of
     them and the in-flight window is keyed by the first. A timestamp consumed
     without an entry (cluster time, a commit stamp) leaves the tail where it
     was. At open the clock resumes from the newest row and from WiredTiger's
     recovery, stable and oldest timestamps (a commit may not go behind them: a
     reopen within the second of the last write, with only catalog writes since
     the last entry, refused to open with EINVAL) and is then bumped one second
     past `max(those, now)`: cluster-time and commit-stamp mints leave
     no row, and a write at a timestamp older than one a data table already
     holds is refused by WiredTiger.
   - The sync/in-flight visibility floor is unchanged in kind and keyed by
     timestamp. The pruner deletes from the one table and records the newest
     position it pruned (`secantus_oplog_meta`'s `pruned` row, written in the
     transaction that removes the rows; 4b-6 must find it a home). That marker
     replaces `floor == 1` as "the oplog still reaches genesis": v1 restore
     refuses a pruned source, and a PITR segment's file name carries `prev`,
     the position it follows (`oplog-<first>-<last>-<prev>.seg`), so segments
     chain by abutting and a missing one is a named gap.
   - `find` and a tailable cursor on `local.oplog.rs` scan the real table
     (`scan_docs_after_recordid`, `collection_min/max_recordid`), a view
     entry's resume position being the last row it covers.
   - Not done here, left to the next stage: the stored fields (`rw`, `bg`,
     `pi`) and the `oplog_view_entries` collapse; `options.size` of the oplog
     is still entries x 16 KiB (4b-5); `oplog_pruned_through` and the
     retention cap are entry-count based (4b-5).

   **As built (4b-2, still format 5):** what is stored is what mongod logs.
   - The entries are built when they are written (`oplog_view_entries`, now
     the writer's, not a view): a plain insert as it is, `ts, t, v, wall`
     appended; a batched insert or delete, and a transaction, as one `applyOps`
     entry at the first of the timestamps they were minted; an update's `o` is
     mongod's `$v: 2` delta; a delete has no `o2`; a retryable write names
     `lsid, txnNumber, stmtId, needsRetryImage, prevOpTime` (the chain kept in
     memory and, after a restart, read from the session's
     `config.transactions` row). `rw`, `bg` and `pi` are gone, and
     `find` on `local.oplog.rs` returns the stored rows. A transaction over
     16 MiB less 16 KiB is mongod's `partialTxn` chain (16 + 4 of twenty 1 MB
     inserts, as measured), `count` on the last.
   - Readers take one operation at a time through `oplog_entry::unwind`: an
     entry's operations (an `applyOps`' elements), each with its entry's
     fields, its place in the entry (`applyOpsIndex`) and, for an update, the
     `updateDescription` its delta describes -- `secantus_core::diff`
     reads the delta back with the code that makes the descriptions
     (`update_description_from_oplog_diff`, `apply_oplog_diff`), as mongod's
     change stream builds its event from the delta alone. Change streams,
     replay, crash recovery, the pre-image lookup and the PITR carry-over use
     it. A pre-image is found by `(ui, entry ts, applyOpsIndex)`, so the `pi`
     field was never needed.
   - A change-stream event inside an `applyOps` has `txnOpIndex` in its resume
     token (`x`); a resume from it reads the entry again and skips the events
     before it.
   - Every user transaction, the PostgreSQL server's included, holds its
     entries on its handle to its commit (`pending_async`, which is also its
     write set and the READ COMMITTED overlay's source), then logs them as one
     entry under a session of its own (`oplog_session`: a synthetic lsid, as
     mongod's entry names one). `transactions_logged_at_commit` is gone: it is
     always so. An open transaction therefore mints nothing and pins nothing.
   - PREPARE TRANSACTION writes mongod's prepare entry (`o: {applyOps,
     prepare: true}`) at the prepare, in a transaction of its own, and records
     its position (`prepareTs`) and session in the prepared-xact row, which
     keeps the write set for a restart; COMMIT PREPARED writes
     `{commitTransaction: 1, commitTimestamp: <prepare ts>}` in the
     transaction that commits, ROLLBACK PREPARED writes `abortTransaction`,
     each with `prevOpTime` the prepare entry. "Already committed" is the
     presence of that commit entry.
   - DDL: `dropDatabase` has `fromMigrate: false`; `create`, `collMod`,
     `dropIndexes`, `rename` and `drop` were already in the measured shapes.
     **An index on a collection that has documents is a two-phase build**, as
     measured: the build's row inserted into `config.system.indexBuilds`,
     `startIndexBuild`, the row updated with `commitReadyMembers`,
     `commitIndexBuild`, the row deleted -- five entries, the rows real writes
     to a real collection made before the first index is (unlogged, as mongod
     has it from its start). Several indexes of one `createIndexes` are ONE
     build: one row, one start and one commit listing every spec (probed 8.2.11;
     fixture step "two indexes"). An index on an **empty** collection is the one
     `createIndexes` entry (probed 8.2.11, 2026-11-21). Replay reads the commit
     as the `createIndexes` it was and the start as nothing. **DDL inside a
     transaction** (probed 8.2.11: an explicit `create`, and the implicit one
     of an insert into a new collection) is a member of the transaction's
     `applyOps`, `{op: "c", ns: "app.$cmd", ui, o: {create, idIndex}}` before
     the insert; here a transaction's DDL entries are stamped with its session
     like its writes and grouped the same way. A name that is not UTF-8 is
     logged as its bytes (also on the plain-insert fast path, which now leaves
     an escaped name to the builder) and escaped again for the engine's readers
     (`oplog_entry::unwind`).

   **As built (4b-4, still format 5; no stored byte changes):** a token is
   mongod 8.2.11's, measured (`tests/fixtures/storage_model/mongod-8.2.11-resume-tokens.json`,
   from `tools/probes/resume_tokens_fixture.py`).
   - `_data` is the hex KeyString (V1, all ascending) of `clusterTime`
     (Timestamp, the entry's `ts`), `version` (int 2), `tokenType` (int: 128 an
     event, 0 a high-water mark), `txnOpIndex` (int, the `applyOpsIndex`; 0
     for a plain entry), `fromInvalidate` (bool), `uuid` (BinData 4 of the
     entry's `ui`, null for none: a high-water mark, `dropDatabase`),
     `eventIdentifier` (an event's only: `{operationType, documentKey}` of a
     write, `{operationType, operationDescription}` of a command that has one
     -- a rename's `{to, dropTarget?}` also on a stream that does not show
     it; `{operationType}` for `drop`, `dropDatabase`) and `fragmentNum` (int
     from 0, a fragment of a split event). `_typeBits` is the KeyString's
     TypeBits, present when not all zero (`820001` for a long `_id`, `828000`
     a double). The `invalidate` event's token is the event it follows with
     `fromInvalidate` true. The same inputs give mongod's bytes
     (`crates/secantus-storage/tests/resume_tokens.rs`); the live comparison
     is `tests/mongod_live/test_resume_tokens.py`.
   - A resume reads from the token's `clusterTime`; an event token's event
     (found by its bytes and `txnOpIndex`) must be the first the stream
     finds, else 280; a high-water mark names no event and reads entries at
     or after its time; an `invalidate` token (`startAfter` only) starts after
     its entry; `resumeAfter` it is 260. An event token with a null `uuid`
     on a single-collection stream is 260, a high-water mark's is accepted.
     A token of another version, or that does not decode, takes
     `resume_keystring`'s answer (mongod's own for garbage), as before: the
     file keeps its job for what this server cannot read, no longer for what
     it writes.
   - mongod's `postBatchResumeToken`, measured: a new stream's mark is the
     entry after the oplog's last (`ts + 1`); one that has read on is the
     last entry read; when that entry was an event it is the event's token
     (a resume's own token included).
   - Nothing stores a token (pre-images and `config.transactions` hold
     none), so there is no format bump.

   **As built (4b-5 and 4b-6, store format 6):** measured on mongod 8.2.11
   (`tools/probes/mongod_replset_state.py`, fixture
   `tests/fixtures/storage_model/mongod-8.2.11-replset-state.json`).
   - **The oplog is held to its size in bytes.** `options.size` of
     `local.oplog.rs` in its `_mdb_catalog` entry is the size: 201,326,592 on
     this box, mongod's macOS default (`getNewOplogSizeBytes`: 192 MB there;
     elsewhere 5% of the free space of the data directory's filesystem, at
     least 990 MB and at most 50 GB -- that branch is mongod's source, not
     measured here, and `default_oplog_size` carries it). A new store takes
     `--oplog-size-mb` (mongod's `--oplogSize`, `[oplog] size_mb`); an
     existing store keeps what its entry holds. `replSetResizeOplog` rewrites
     the entry (`size` megabytes times 1,048,576, the fraction kept: `1000.5`
     gives `maxSize` 1049100288), which `collStats.maxSize` and
     `listCollections` read. The entry-count cap (`max_entries`,
     `--oplog-max-entries`, `[oplog] max_entries`, the 16 KiB-an-entry
     arithmetic) is gone.
   - **No write counts the table.** `OplogState.live_count` / `live_bytes`
     are mongod's truncate-marker totals: read at open (the size storer's
     `oplogSize` beside its clean-close marker, else one walk, as mongod
     builds its markers by scanning when it has no size storer), added to when
     a write COMMITS (each in-flight window carries the entries and bytes it
     wrote and `deregister_in_flight(.., committed)` / `resolve_minted` add
     them: a rolled-back statement or a retried write conflict adds nothing)
     and taken from by a truncation. The sweep reads `live_bytes` for the
     excess, then walks the oldest entries reading only their sizes
     (`with_value_u`, no copy) until the entry that crosses the excess, so
     the oplog ends at or under its size, in whole entries. Behind it, as
     before, the retention window and `minRetentionHours`; the sweep never
     goes past the stable checkpoint's timestamp in a store whose data tables
     are not logged. Measured: mongod keeps entries past its size until a
     stable checkpoint passes them (a 1 MB oplog held 4.9 MB for the first
     minute), then holds 972,645 bytes of a 1,048,576 oplog, whole entries.
   - **Pre-images** expire against the oplog's oldest entry, as mongod's
     remover does (`_id.ts` before it, and at or below the all-committed
     point), not against a position remembered from the sweeps of this run.
   - **`secantus_oplog_meta` is gone, all three rows.** `oplog` (ident and
     uuid): the oplog is created at store creation, its catalog entry made
     durable by a checkpoint taken before the store serves
     (`Storage::genesis`), which is what mongod's `replSetInitiate` does
     (measured: the oplog's first entry is `{op: "n", ns: "", o: {msg:
     "initiating set"}, ts, v, wall}`, no `t`, then the `local.replset.*`
     collections, a checkpoint). The catalog is not logged in a store whose
     data tables are not; with the entry in the first checkpoint, a kill -9
     before the next one finds the oplog by its catalog entry and replays its
     logged entries. `pruned` (newest position pruned): mongod has no such
     mark. What it has is the first entry, `initiating set`; an oplog whose
     oldest entry it is has lost nothing from the front
     (`oplog_reaches_genesis`, replacing `oplog_pruned_through`). A PITR
     segment's `prev` is 0 for that oldest entry, else the end of the newest
     segment in the archive, else `first - 1` (not known); see the open item
     in the backlog. `stable` (`stable_seq`, `data_nonlogged`): the recovery
     point is WiredTiger's `get=recovery`, the stable timestamp stored by the
     checkpoint itself, which `stable_seq` is initialised from and which a
     stable checkpoint moves; the mode is read from the tables' own metadata
     (it already was; the row's copy was never the authority).
   - **The replica-set state** a new store writes at its initiation, shaped
     and typed as measured (fixture): `local.replset.minvalid` `{_id: ObjectId,
     t: -1 (long), ts: Timestamp(0, 0)}` (not logged),
     `local.replset.oplogTruncateAfterPoint` `{_id:
     "oplogTruncateAfterPoint", oplogTruncateAfterPoint: Timestamp}`,
     `local.replset.election` `{_id: ObjectId, term: 1 (long),
     candidateIndex: 0 (long)}`, `local.replset.initialSyncId` `{_id: UUID,
     wallTime}`, `local.system.rollback.id` `{_id: "rollbackId", rollbackId: 1
     (int)}`, and `config.system.indexBuilds` (a `create` entry follows the
     initiating no-op, as mongod's does; 4b-2 made it lazily, unreplicated). `local.system.replset` is the server's
     (`record_replset_config`), which has the set's name and address. Their
     tables are logged in a store whose data tables are not, except `minvalid`
     (measured: every `local.replset.*` and `local.system.*` table
     `log=(enabled=true)`, `minvalid` `false`). `rollbackId` is 1 higher
     after an unclean shutdown (measured: 2 after the first kill -9, 3 after
     the second; a clean restart leaves it); here only a store with a lock
     file (the servers) can tell.
   - **Restore with the oplog carried over** (`import_oplog_segment`) takes
     the source's timeline as it is: the target's own initiating no-op and
     `indexBuilds` creation, which it wrote when it was made and are dated
     after everything carried, are removed first, so the carried oplog reaches
     genesis exactly when the source's did.
   - **Left open (backlog):** the election `term` and a `new primary` entry
     on each start; `oplogTruncateAfterPoint` is refreshed at each stable
     checkpoint and at close, not every 100 ms, and recovery does not
     truncate after it (mongod's acknowledgement waits for the journal
     flusher's cycle, ours for the commit's own sync, so truncating would lose
     acknowledged writes); `--oplog-retention-seconds` (default 3600) is a
     time limit mongod does not have; a PITR chain cannot prove that nothing
     was truncated between two archived segments without a mark of its own.

   **Landing:** 4b-1 to 4b-3 cannot land apart (the table, what is in it
   and every reader of it), so they land together as store format 5, with
   the crash tests: kill -9 at random points of a write burst, many times,
   then reopen and check that every acknowledged write is present and that
   no unacknowledged one is half-applied. 4b-4 lands alone; it changes no
   stored byte. 4b-5 and 4b-6 land together as format 6. Each PR runs the
   phase gate plus `do-perf` at 1, 4 and 16 writers against `main`.
5. **Timeseries buckets.** Store time-series data as `system.buckets`
   bucket documents in a clustered collection, as mongod does; remove the
   framed value.

   **As built (phase 5, store format 7):** measured on mongod 8.2.11
   (fixture `mongod-8.2.11-layout.json`, probes `tools/probes/` and the live
   test `tests/mongod_live/test_clustered_collections.py`).
   - **The value is the document.** `frame_doc_value`, `unframe_doc_value`
     and the time-series key suffix are gone. The frame's `id_key` served
     only the transaction undo log and the PostgreSQL read overlay's keys;
     both derive it from `_id` when they write (`id_key_of_blob`), so a read
     pays nothing and a scan strips nothing.
   - **A clustered collection** (`clusteredIndex`, `system.buckets.*`,
     `config.system.preimages`) has a table with `key_format=u` whose key is
     the KeyString of `_id` (no TypeBits, no end byte), the document as the
     value, and no `_id_` table, `idxIdent` entry or `md.indexes` row. The
     placement (`secantus_idents`) records `clustered`; the table is made
     with it. `system.buckets.*` is `block_compressor=zstd`.
   - **`RecordId`** (`secantus_core::recordid`) is `Long(i64)` or
     `Bytes(Vec<u8>)` wherever a collection's rows are named: the doc table
     cursor, scans, candidates, index entries (a clustered collection's end
     in `append_record_id_bytes`: key, end byte, the bytes and their size),
     the `Storage` trait, the explain stream, `$recordId` (`Int64` or BinData)
     and the spill members of a sort (`RID_AND_OBJ` holds the format byte, a
     four-byte length and the bytes; measured by `spilledBytes`). Capped and
     oplog positions remain counters.
   - **Behaviour:** a duplicate `_id` is the row that already holds the key;
     an `_id` range reads the table between the span's keys; pre-images are
     read by `{nsUUID, ts, applyOpsIndex}` and expire per collection run;
     the clustered `create` oplog entry carries `clusteredIndex` and no
     `idIndex`; `collStats.indexSizes` leaves out the clustering key while
     `nindexes` counts it.
   - **Comparison tool** (`tools/probes/mongod_store_layout.py --secantusd`,
     2026-11-21, now tolerant of a store with no `sizeStorer`): every table
     both stores hold has the same file config, `app.clustered`,
     `app.system.buckets.ts` and `config.system.preimages` among them
     (`key_format=u`, snappy / zstd / snappy), their row counts agree, and
     the clustered keys are mongod's byte for byte (`2b02`, `3c7300`,
     `64`+ObjectId; the pre-image key's layout). Tables only mongod has:
     `admin.system.{keys,users,roles}`, `<db>.system.views`, `sizeStorer`
     and the build's `internal-*`; tables only this server has: the
     `secantus_*` private tables, `app.ts` (and its index), the view `app.v_view`'s
     `_id_`.
   - **Still different from mongod after this phase** (for 6, 7 and the
     backlog): the time-series collection's own namespace still has a table
     and registry entry (mongod has the view and the buckets only); the
     buckets collection's catalog options lack the `validator`,
     `timeseries` and `timeseriesBucketsMayHaveMixedSchemaData` mongod
     keeps; explain names `IXSCAN _id_` for a clustered collection where
     mongod says `CLUSTERED_IXSCAN` / `EXPRESS_CLUSTERED_IXSCAN`; views are
     still in the registry (phase 7); the size storer is still keyed by
     namespace (phase 6, done).
6. **Size storer and the PostgreSQL server.** Key sizes by ident.
   `pg_relation_filepath` (secantus-pg/src/lib.rs:22636) returns the ident's
   file. Confirm the SQL-to-collection hand-off test still passes.

   **As built (phase 6, store format 8):** measured on mongod 8.2.11 with
   `wt dump -x table:sizeStorer` on stores it left after a clean stop, a
   rename and two `kill -9` runs (the fixture's rows, and four more stores).
   - **mongod's form.** `table:sizeStorer` is created `key_format=u,
     value_format=u` and otherwise WiredTiger's defaults (no compressor,
     4KB internal / 32KB leaf pages, `prefix_compression=false`,
     `split_pct=90`, empty `app_metadata`, logged). The key is the ident's
     table URI as raw bytes, no terminator (`table:collection-<UUID>`,
     `table:_mdb_catalog`); the value is the 43-byte BSON `{numRecords:
     Int64, dataSize: Int64}` (3 documents of 69 bytes are
     `2b000000126e756d5265636f726473000300000000000000126461746153697a6500450000000000000000`).
     A row exists for each record store whose sizes ever moved: a
     collection created and never written has none, one written and
     emptied keeps `0, 0`; the oplog and `_mdb_catalog` have one; **no
     index ident has one** (mongod reports an index's size from its file).
     A rename keeps the ident and so the row, unchanged (`app.a` renamed
     `app.a2`: same key, same value, same row count); a drop takes the row
     away when the ident is dropped (a drop-pending ident keeps its row).
   - **When mongod writes it:** at its checkpoints and at a clean stop. Killed
     3 s after 1,000 inserts, its size storer had no row for the collection
     or the oplog; after the restart it counted 1,001 (replication recovery
     adjusts a collection's counts as it replays), but its **oplog** row
     said 1 entry / 101 bytes while the oplog held 21 / 127,650: mongod does
     not correct the oplog's own count after a crash. Killed 75 s later (a
     checkpoint between), the rows were current.
   - **This server** (`sizes.rs`) keeps the counts in memory by namespace,
     as before (transactional adjust, rollback undo, a rename moves them
     inside its transaction), and at a clean close writes them to
     `sizeStorer` by ident through the placements: a row for every
     collection with a slot or an existing row, the oplog's, and
     `_mdb_catalog`'s (counted at close); rows of idents the catalog no
     longer names go. What mongod's table does not hold is in
     `secantus_size_storer` (`key_format=u`): each index's entry bytes keyed
     by the index's ident URI (`collStats.indexSizes` reports entry bytes,
     RM-040), and the clean-close marker with the oplog fingerprint. Both
     tables are written in one transaction. An unclean open still walks
     every table once (exact for the oplog too, where mongod's count is
     stale); sizes for a namespace or index with no ident leave the marker
     out, so the next open walks rather than trust them.
   - **Store format 8**; formats 4-7 are refused by name.
   - **Comparison tool** (`--secantusd`, 2026-11-21; the phase 5 tolerance of
     a store without `sizeStorer` is gone, and rows are labelled by
     namespace): `sizeStorer` has the same table and file config in both
     stores, and every collection both stores hold has the same row
     (`app.c` 7 / 339, `app.ks` 38 / 1,761, `app.system.buckets.ts` 2 / 530,
     `config.system.preimages`, `config.system.indexBuilds` 0 / 0, the
     `local.replset.*` rows and the rest). Rows only mongod has: the
     collections only it has (`admin.system.{keys,users,roles}`,
     `app.system.views`) and the drop-pending `todrop` (this server had
     reaped its table before the stop). Rows that differ: `local.oplog.rs`,
     `local.startup_log`, `local.system.replset` and `_mdb_catalog`, whose
     contents differ.
   - **`pg_relation_filepath`** already named the ident's file
     (`collection-<UUID>.wt`, `index-<UUID>.wt`, relative to the data
     directory, as RP-112 decided). PostgreSQL 15.19 answers
     `base/<dboid>/<relfilenode>` (`base/5/1438623`); the form stays ours.
     Two bugs fixed: the primary key answered NULL (it is the storage's
     `_id_` index), and a name was resolved only among tables and views, so
     an index's, a sequence's or a catalog's name was 42P01 (the argument is
     a `regclass`, read now as `regclassin` reads it). Filed: a catalog's
     file is NULL (RP-1075, PostgreSQL gives `base/5/1259`), every sequence
     names one shared file (RP-1076), `pg_relation_filenode` is refused
     (RP-1077).
   - **Still different from mongod after this phase:** the size storer is
     written only at a clean close, not at checkpoints, so a store that
     crashed holds the last clean close's rows until it is opened again; an
     emptied collection that is dropped and re-created under the same name
     in one run gets a `0, 0` row mongod would not write; the oplog's count
     after a crash is exact here and stale on mongod.
7. **mongod opens the store.** A test creates a store with SecantusDB holding
   everything phase 0b created, stops it cleanly, starts mongod 8.2.11 on the
   directory with `--replSet` (decision 8), waits for it to become
   primary, and runs `validate({full: true})` on every collection, then
   reads every document, walks every index and opens a change stream from a
   resume token SecantusDB issued.

   **The reverse test** (decision 10): mongod 8.2.11 writes the phase 0b
   store and stops cleanly; `secantusd-mdb` opens the directory, reads every
   collection and index, and writes to it; then mongod opens it again and
   `validate` passes. What SecantusDB refuses at open is listed here and in
   the backlog: at least a replica-set config with more than one member,
   any index kind phase 4 refuses, and any catalog option it does not
   implement. An unclean mongod shutdown (journal and oplog recovery
   pending) is tested too, since recovery is now mongod's model.

   **As built (phase 7, forward direction, store format 9).** mongod 8.2.11
   on a store `secantusd-mdb` wrote and stopped cleanly, what it refused, in
   order (each read from its log, fixed, then the next):
   - **The set's name and address.** Started `--replSet rs0` on another
     port, mongod leaves the store REMOVED ("NodeNotFound", "set name
     differs"): its own store does the same. The test starts it as the
     store's set (`secantus`) on the port its one member names. Not a store
     difference.
   - **`oplogTruncateAfterPoint` left set by a clean close** (the
     all-committed point, one past the last oplog entry). mongod's clean
     stop leaves `Timestamp(0, 0)` (measured). Set, mongod truncates after
     the last entry, forces stable and oldest back to it, keeps its own
     oldest from the checkpoint (stable minus 300 s), and its next
     `oldest_timestamp` goes backwards: invariant, abort. `Storage::close`
     now clears it (`clear_oplog_truncate_after_point`).
   - **A view in `_mdb_catalog`** ("'viewPipeline' is not a valid
     collection option"): a view had a placement, a table and an `_id_`
     table. On mongod a view, and a time-series collection, is a
     `<db>.system.views` document and nothing else (measured: `{_id, viewOn,
     pipeline, collation?}`; a time-series one `{viewOn:
     "system.buckets.<n>", pipeline: [{$_internalUnpackBucket: {timeField,
     metaField?, bucketMaxSpanSeconds}}]}`). Now a view-defining namespace
     keeps its registry row (the working set) and is unplaced at create;
     `system.views` is a real collection written beside each view DDL and
     brought in line at every open (`sync_system_views`; the shapes are
     `secantus_core::viewdefs`); `system.buckets.<n>`'s catalog entry has
     mongod's options (`uuid, validator, clusteredIndex: true,
     expireAfterSeconds?, timeseries`) and
     `md.timeseriesBucketsMayHaveMixedSchemaData: false`.
   - **A multikey index** failed `validate`: six extra entries (the
     whole-array keys, phase 4b's note) and "multikey paths do not cover"
     (an index built over existing documents recorded the flag, not the
     paths). Entries are mongod's keys alone now; an equality to an array
     probes the array and its first element (`undefined` for an empty one),
     the points of mongod's bounds, in `scan_bounds::equality` and in the
     prefix scans (`scan_index_for_id_keys`). Builds record `multikeyPaths`
     (`BuildIndex::note_multikey`, `plan_index_entries`). A `2d`, `hashed`,
     text or wildcard index's catalog entry has no `multikeyPaths` and
     mongod's flag: a wildcard index with paths made mongod's planner trip
     on every query of its collection.
   - **A killed store** (found probing the same forward direction after a
     SIGKILL): its checkpoint lacked the feature-compatibility version
     (written by the server after the initiation's checkpoint; fassert
     40652) and `local.system.replset` (made at bind; mongod started
     uninitiated and dropped every collection made since as unknown idents).
     The store's initiation now writes the FCV before its checkpoint, and
     recording a new set configuration takes a checkpoint, as
     `replSetInitiate` does.

   With these, `test_mongod_opens_the_phase_0b_store` passes: phase 0b's
   scenario written by `secantusd-mdb`, mongod primary on it, `validate`
   clean on every collection (the `local` ones mongod itself keeps busy
   warn as they do on its own store, measured), every document and hinted
   index walk byte for byte as SecantusDB served it, and a change stream
   resumed from a SecantusDB token. Store format 9 (views, whole-array keys).

   **Still different from mongod, filed:** `2d` keys (RM-741), `2dsphere`
   keys (RM-742) and wildcard keys (RM-743), which the scenario's indexes of
   those kinds are dropped for (hashed and text are refused at create);
   users and roles in private tables (RM-744); a view's DDL logged as
   commands, its `system.views` writes unreplicated (RM-745); after a crash
   the recovery timestamp can pass the oplog's end, which aborts mongod
   (RM-746). **Decided (Joe, 2026-11-21): option A** -- every timestamped
   write to an unlogged table gets an oplog entry at its timestamp (or
   becomes an untimestamped write to a logged table), as mongod; the
   stable timestamp is the newest committed entry. Built in the next
   slice, with the reverse direction. The SQL server's
   store opens in mongod too now; its documents carry the `__sys_*` columns
   (RP-1074).

   **The reverse direction is not built** (RM-723 stays open). Today
   `store_format::prepare` refuses a mongod store by name and touches
   nothing. What opening one needs, measured on mongod 8.2.11 stores
   (`tools/probes/mongod_store_layout.py`, the fixtures):
   - accept a store with `storage.bson` and no `secantus.format`, and write
     the format file only once the open has succeeded;
   - rebuild this server's registry from `_mdb_catalog`: a
     `secantus_collections` row per catalog entry (its options), a
     `secantus_idents` placement per entry (`ident`, its row id,
     `idxIdent`, clustered from `clusteredIndex`), a `secantus_indexes` row
     per `md.indexes` element with the stored options this server keeps
     (`multikey`, `multikeyPaths` as path strings, the build stamps),
     view and time-series rows from `system.views`, and the time-series
     collection's own index rows from its buckets' (the inverse of
     `ts_buckets::bucket_key`);
   - refuse by name, before writing anything: a `local.system.replset`
     with more than one member; an index of a kind this server does not key
     as mongod does (`2d`, `2dsphere`, wildcard until RM-741 to RM-743,
     `hashed`, text); any catalog option or index option it does not
     implement; a store at an FCV other than 8.2;
   - size counts from mongod's `sizeStorer` with no clean-close marker (one
     walk, as an unclean open does now);
   - users and roles from `admin.system.users` and `admin.system.roles`
     (RM-744);
   - recovery of a killed mongod's store: WiredTiger recovers the logged
     tables; the oplog after `get=recovery` is replayed, mongod's
     `ReplicationRecovery` (truncating after `oplogTruncateAfterPoint`
     first), which needs replay of every entry form mongod writes
     (`startIndexBuild`/`commitIndexBuild`, `new primary` and periodic
     no-ops, `applyOps`, `create` with `idIndex`);
   - a set name other than `secantus`: the server takes the stored one.
   Gap tests: `test_secantus_opens_a_mongod_store`,
   `::test_secantus_opens_a_killed_mongod_store`,
   `::test_secantus_refuses_a_set_of_two_members_by_name`,
   `::test_secantus_refuses_an_index_kind_it_does_not_serve_by_name`.

   **As built (phase 7b, RM-746 and the reverse refusals; store format 9,
   unchanged).** The layout did not change, so the format stays 9; what
   changed is which timestamp each write commits at, and the timestamps a
   checkpoint and the truncate point name.
   - **What mongod logs an entry for** (8.2.11, measured 2026-11-22 on a
     one-member set): every DDL (`create`, `createIndexes`, a build's
     `startIndexBuild` / `commitIndexBuild` with its `config.system.
     indexBuilds` insert, update and delete, `collMod`, `dropIndexes`,
     `drop`, a view's insert into `system.views`), every replicated
     document write (a retryable one with `lsid`, `txnNumber`, `stmtId`), a
     transaction's `applyOps`, `config.system.sessions` inserts. No entry
     for: the FCV document at initiation, a session's `config.transactions`
     row, `config.image_collection`, `system.profile`, `replSetResizeOplog`,
     a `setFeatureCompatibilityVersion` to the version it has, a retryable
     update that matched nothing. mongod writes the first group at their
     entries' timestamps (one unit of work) and the second untimestamped.
   - **The stable timestamp is the newest oplog entry at or below the
     all-committed point** (`advance_timestamps`, one cursor `search_near`
     on the oplog per 250 ms tick; nothing on the write path). A
     cluster-time mint and a write without an entry no longer move it, so
     a checkpoint's recovery timestamp is always an entry's.
   - **`oplogTruncateAfterPoint` is the newest entry at or below the
     all-committed point, moved before every journal sync** (the 100 ms
     flusher, a journaled write's wait and each stable checkpoint), as
     mongod's journal flusher moves it; written in place into its logged
     `local` table. mongod truncating after it after a kill keeps every
     entry the journal held. (RM-736's other half, this server's own
     recovery truncating after the point, stays a decision.)
   - **`reserve_stamp` is gone.** A write that logs no entry commits
     untimestamped (`begin_transaction(no_timestamp=true)` where it is
     known in advance: `unreplicated()`, the new `untimestamped()` guard,
     the oplog off), except inside an operation's scope (`Storage::
     op_scope`): there it commits at the newest timestamp the operation's
     entries committed at, which each entry holds above the stable
     timestamp until the operation ends. Scopes: a retryable write (its
     `config.transactions` row and `findAndModify` image), `commitTransaction`
     (its row), `drop`, `dropDatabase`, `renameCollection`, a view's create
     and `collMod`, an index build's begin, commit and abort. Untimestamped:
     what the store writes as it opens, catalog and user-record writes
     (`catalog_write_on`), an index build's keys and side-write drains (as
     mongod's bulk load; a cut-short build restarts from an empty table),
     `system.profile` (now `unreplicated()`), `replSetResizeOplog`, oplog
     and pre-image pruning, a PITR import's deletions, `repair`. The oplog
     entries a transaction of their own writes (the initiating entry, a
     prepared transaction's entries and abort) commit at their own
     timestamp. A replicated statement that ends with no entry and wrote a
     row an earlier write timestamped is refused by WiredTiger; that is
     reported as an internal error naming RM-746's rule, never a retried
     write conflict.
   - **Found on the way: a nested statement transaction dropped the outer
     one's minted ranges.** `log_index_build` inserts its `config.system.
     indexBuilds` row through the public API inside its own statement; the
     inner statement deregistered the outer one's oplog ranges before the
     outer transaction committed (the visible tail could pass entries not
     yet committed) and the outer one then committed with no entry of its
     own. The inner statement now keeps the outer ranges aside and gives
     them back.
   - **Killed under load, mongod opens the store**: twelve rounds of eight
     concurrent workloads (inserts, `j: true` inserts, updates, retryable
     `findAndModify`, transactions, collection and index DDL, deletes, bulk
     inserts with index builds), SIGKILL at 0.5-4 s or past the first
     stable checkpoint: mongod 8.2.11 primary every time, `validate` clean
     after an `fsync` (before it, a table recovery wrote warns "actively in
     use" on mongod's own killed store too, measured), no `j: true` write
     lost. Views excepted: mongod's recovery fails on our view `create`
     entry (RM-745).
   - **The reverse refusals.** `store_format::prepare`, finding a mongod
     store, opens WiredTiger read-only and names in its refusal what this
     server could not serve: a `local.system.replset` of more than one
     member, an index of a kind it does not key as mongod does (`2d`,
     `2dsphere`, `hashed`, text, wildcard, columnstore), a collection or
     index option outside what it keeps, an FCV other than 8.2, documents
     of `admin.system.users` / `roles` (RM-744: this server does not read
     them, so they are refused, not skipped). A killed mongod's store needs
     log recovery, which read-only WiredTiger will not run: it is refused
     without the list. `test_secantus_refuses_a_set_of_two_members_by_name`
     and `test_secantus_refuses_an_index_kind_it_does_not_serve_by_name`
     pass (the helper's mongod stop now falls back to `shutdown` with
     `force`: mongod refuses a plain one while its set names another
     member, and the test had been killing it after 60 s).
   - **Filed** (the same writes run on both, what each logged compared):
     a read-only transaction's commit logs no `w: "majority"` no-op
     (RM-747); the initiation logs no `create` of `config.system.preimages`
     (RM-748). RM-745 now names what it costs a killed store; RM-736 keeps
     only its decision (this server's own recovery truncating).
   - **Not built: opening and serving a mongod store, clean or killed.**
     What remains is the list above ("What opening one needs"), unchanged,
     plus: the format file is written only once the open has succeeded
     (mongod ignores it, so the store stays mongod-openable). Still to
     measure when it is built: whether mongod's `admin.system.keys` and
     `config.system.sessions` serve as ordinary collections here; whether
     its drop-pending idents (tables no catalog row names, which this
     server's `reconcile_idents` drops at open) are what mongod's own
     restart drops; whether mongod's `local.replset.*` and `minvalid`
     documents read as this server's (mongod reads ours, phase 7). The
     killed store's recovery is mongod's
     `ReplicationRecovery` on our replay: truncate after the point, then
     replay from the stable timestamp every entry form mongod writes (the
     `create` with `idIndex`, `startIndexBuild` / `commitIndexBuild` with
     their `config.system.indexBuilds` writes, `applyOps`, the `new
     primary` and periodic no-ops), compared document for document with
     mongod's own restart on a copy of the same store.
   **As built (phase 7c: RM-736, and mongod's stores opened; store format
   9, unchanged).** The layout did not change; a store mongod made gets
   `secantus.format` once this server has opened it.
   - **Recovery truncates the oplog after the point** (RM-736, decided by
     Joe 2026-11-22: "Truncate as mongod"). A store whose data tables are
     not logged, opening, reads `local.replset.oplogTruncateAfterPoint`
     before it reads the oplog: `Timestamp(0, 0)` truncates nothing; a
     point at or below the stable timestamp truncates after the stable
     timestamp; otherwise every entry after the newest one at or below the
     point goes, and when the stable timestamp is past that entry the
     stable and oldest timestamps move back to it with `force`. Then the
     point is cleared and the journal synced, and the replay runs from the
     stable timestamp (mongod's `_truncateOplogIfNeededAndThenClearOplog
     TruncateAfterPoint`; its fatal 40296 when no entry is at or below the
     point). Measured on mongod 8.2.11 with the journal flusher paused
     (`pauseJournalFlusherBeforeFlush`, which this server now honours under
     `--enable-test-commands`): after a SIGKILL two `w: 1, j: false`
     writes are gone, entries and documents, and the journaled one stays;
     with the flusher running a second, all three stay. This server gives
     both answers. A journaled or majority write's wait moves the point
     first (phase 7b), and now waits first for every entry minted before it
     to commit, as mongod's acknowledgement waits for its durable point
     (`lastDurable`) to pass the write with no hole before it: else the
     point it moved could sit below a write a concurrent earlier entry had
     not yet committed under. So does every acknowledged write of a store
     started with `--sync-on-commit`, a mode mongod does not have, whose
     acknowledgement means the journal holds the write (without it, a kill
     under load lost 7 to 19 of about 200 acknowledged `w: 1` writes, three
     runs of three). The PostgreSQL server syncs every commit but its
     store's data tables are logged (no truncation) unless
     `SECANTUS_DATA_NONLOGGED` asks otherwise; with it, a commit is not
     waited for as a journaled write is.
   - **The registry follows the catalog.** `store_format::prepare` answers
     `StoreKind::Mongod` for a store with `storage.bson` and no format file
     (judged read-only first when mongod stopped cleanly, i.e. left
     `mongod.lock` empty; a killed one is judged once WiredTiger has
     recovered it). As any store opens, `adopt::reconcile` compares this
     server's registry with `_mdb_catalog` where another server may have
     written it: always for a store mongod made, for one of this server's
     whose clean-close marker no longer matches the oplog (mongod opened it
     since), and, after an unclean close, when the placements and the
     catalog's idents disagree. Then the catalog wins: each namespace it
     lists gets its registry row (its options), placement (`ident`, the row
     id, `idxIdent`, clustered from `clusteredIndex`) and index rows (the
     spec's options, `buildSeq` from the listing order, `collationKeys` for
     a collated index, `multikey` and the `multikeyPaths` bytes as path
     prefixes); an index whose listed spec is unchanged keeps what this
     server recorded (a PostgreSQL index's BinData order, say) with
     mongod's multikey paths added; a namespace or index the catalog no
     longer lists goes; view rows come from each `system.views`. Before
     writing, `store_format::catalog_findings` refuses by name what this
     server cannot serve (the 7b list, plus an index whose build has not
     finished, an index of another version than 2, and a time-series
     collection). Found on the way, and the reason this runs for this
     server's stores too: **mongod's writes to a store this server wrote
     were lost when this server opened it again** (RM-757, measured: the
     collection mongod made had its tables dropped as orphans, the one it
     dropped was still served).
   - **Opening and serving a mongod store, clean.** Phase 0b's scenario
     written by mongod (less the index kinds RM-741 to RM-743, hashed and
     text, the time-series collection, users and roles) and stopped
     cleanly: every document and every hinted index walk of its 22
     namespaces reads byte for byte as mongod served them, a change stream
     resumes from a token mongod issued, the server serves the store's set
     under its own name (`rs0`), and mongod opening the store again after
     this server wrote to it becomes primary and validates every
     collection (`test_secantus_serves_the_phase_0b_store_mongod_wrote`).
     The open questions, measured: `admin.system.keys` (two HMAC keys) and
     `config.system.sessions` (made by mongod's session refresh,
     `logicalSessionRefreshMillis`) serve as ordinary collections; each
     server's session cache rewrites `config.system.sessions` as it runs,
     so the comparisons leave it out. mongod's drop-pending idents (a
     dropped collection's tables) survive mongod's own clean restart (it
     drops only `internal-*` idents at startup, log 22257); this server's
     `reconcile_idents` drops them, which changes nothing either server
     reads. `local.replset.minvalid`, `.oplogTruncateAfterPoint`,
     `.election`, `.initialSyncId` and `local.system.rollback.id` hold the
     documents this server writes, field for field. mongod's restarts raise
     the term (2 after one); this server's entries there say `t: 1`
     (RM-735), and mongod opened such a store again without complaint.
   - **Opening a killed mongod store.** WiredTiger recovers the logged
     tables, the oplog is truncated after the point, and the entries after
     the stable checkpoint are replayed through the ordinary write paths:
     the `create` (with `idIndex`), the two-phase build's `startIndexBuild`
     / `commitIndexBuild` and its `config.system.indexBuilds` writes, a
     transaction's `applyOps`, `$v: 2` updates, deletes, an index drop, a
     capped insert and the `new primary` no-op (mongod 8.2.11 wrote no
     periodic no-op on an idle one-member set in 12 s, measured). The
     store serves what mongod's own restart on a copy of it serves,
     document for document and index walk for index walk, and mongod opens
     what this server wrote after it
     (`test_secantus_opens_a_killed_mongod_store`). Found on the way: **a
     replayed `create` made the collection without the entry's UUID**
     (RM-758), in this server's own crash recovery too: mongod refused the
     store ("MustDowngrade: Collection does not have UUID in KVCatalog") and
     the collection's next write minted another one. And, comparing the two
     servers' recoveries of the same killed store (the phase 7b kill-stress
     probe, extended): **a replayed transaction left no `config.transactions`
     row for its session** (RM-760; mongod's recovery writes `state:
     "committed"` with the entry's timestamp and wall time).
   - **RM-723 is closed**: its last two gap tests
     (`test_secantus_opens_a_mongod_store`,
     `test_secantus_opens_a_killed_mongod_store`) pass, and are
     `backlog_fixed`.
   - **Still refused, filed:** a time-series collection (RM-754); an index
     build mongod left unfinished, which mongod's restart finishes
     (RM-755); a killed mongod store is judged at its stable checkpoint, so
     one whose oplog drops what is refused after the checkpoint is refused
     where mongod serves it (RM-756). Users and roles (RM-744) and the
     index kinds (RM-741 to RM-743) as before. Not covered: a store of
     this server's that crashed and was then written by mongod, where
     mongod changed only options or views (its idents unchanged): the open
     cannot tell that from this server's own crash.
   - **Performance** (local, macOS arm64, release builds, 2026-11-22; the
     droplet A/B is Joe's). Phase 7b's -1.1% / -1.6% (droplet, journaled
     inserts): `sample` of one writer's default-write-concern inserts puts
     `refresh_truncate_point` at 85 of 7,067 samples of `flush_journal`
     (1.2% of a journaled write's wait, 40% of it opening a WiredTiger
     session per write) on a 51bde76f3 build, nothing on 96dcf2fa6; the
     rest of 7b's write path (`no_timestamp` begins, `advance_timestamps`,
     op scopes) is under 0.1% of `dispatch` at `w: 1`. Phase 7c keeps the
     refresh's session between refreshes and flushes with a pooled one:
     14 samples of 7,021. Throughput, five interleaved rounds of 15 s:
     journaled, 1 writer 208.0 / 206.4 / 210.6 ops/s (96dcf2fa6 /
     51bde76f3 / 7c), 4 writers 242.8 / 239.2 / 238.2, within the runs'
     spread (macOS's `F_FULLFSYNC` is ~5 ms a write); `w: 1`, four rounds,
     1 writer about 7,300 and 4 writers about 14,200 ops/s on all three.
     Per-operation paths 7c changed: a journaled write's wait (waits for
     the in-flight entries minted before it, the paused-flusher check, the
     cached refresh session, a pooled session for the flush); every command
     reads `syncs_every_commit` (a field); nothing on the `w: 1` write path.

8. **Clean-up.** Remove the legacy-table readers, the shard routing and the
   format markers this replaces; rewrite the layout tests below. Update
   CLAUDE.md's "One on-disk format" section, the `python-servers` skill's
   layout description, and the docs pages that describe the store.

## Ownership and parallel work

The first two rules were confirmed by Joe, 2026-11-19. The first is an
exception to CLAUDE.md's one-server rule, limited to this plan.

- This change spans both Rust servers, which CLAUDE.md's "one session works on
  ONE Rust server" rule otherwise forbids. **One session owns the whole plan,
  on the MongoDB side**, and makes the shared-storage and `secantus-pg`
  changes itself; each PR says the other server links the change.
- **No `fix_many` lane runs while a phase 0d, 3, 4 or 4b branch is open.** Those
  phases rewrite most of a 31,000-line file the lanes also edit. Phases 2, 5
  and 6 are narrower and can run beside one lane.
- **Filed (phase 0): RM-723** (the layout, both directions; gap tests in
  `tests/backlog/test_rm_storage_model.py`) **and RP-1074** (the PostgreSQL
  server's store; `tests/backlog/test_rp_storage_model.py`). Each phase
  re-marks the gap tests it makes pass as `backlog_fixed`, and the items
  close when the last test passes.
- Each phase is filed in both `tasks/backlog-rust-mongodb.md` and
  `tasks/backlog-rust-postgres.md` with a `Backlog-ID` and a gap test (the
  phase's layout test), per the `backlog` skill, and carries a `changelog.d/`
  fragment. Phase 1's fragment is the breaking-change notice.

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

New tests: a layout test per phase asserting the WT tables a store holds and
their configs, compared against the phase 0 mongod store; crash tests for
drop and rename mid-flight; the refusal message for an old store; the
dump-and-restore round trip for each server.

## Risks

- **WiredTiger version.** mongod 8.2 may not open 7.0.33 files, or may
  upgrade them past what 7.0.33 reads. Phase 0a measures it; phase 0d is the
  answer (decision 9), and is itself a risk to every patcher and to
  durability under group commit.
- **Many WT tables.** A store with thousands of collections and indexes holds
  thousands of files and handles. mongod lives with this through WT's handle
  sweep and its session cursor cache; we will need the same settings and a
  test at, say, 10,000 collections.
- **Drop while in use.** WT refuses to drop a table with open cursors. The
  ident reaper must wait out readers without blocking the drop command, as
  mongod's does.
- **Startup.** Opening is per table, lazily, so startup should not grow; the
  per-phase gate measures it.
- **Concurrency.** Per-collection files are what PR #594 approximated with 16
  shards; the per-phase `do-perf` run must not regress. The compressor change
  (lz4 to snappy) moves this number on its own, so measure it separately.
- **Logging and recovery.** Decision 8 moves durability of user tables from
  the WT log to checkpoints plus oplog replay. That is a recovery redesign,
  not a config flag, and it is where a mistake loses acknowledged writes.
  Phase 4b needs kill-at-every-step crash tests before it merges.
- **Reading mongod's stores.** A mongod store can hold anything mongod can
  write. The open-time check must be complete: a store opened with one
  unrecognised catalog option silently ignored is a wrong answer later.
- **Index kinds.** Matching ICU, S2 and the text format exactly may be out of
  reach for some kinds; those are refused by name, not approximated.
- **Backup and PITR archives** name tables; `pitr_archive.rs`, `repair.rs` and
  `compact.rs` need the new names.

## Not in scope

- Changing WiredTiger's code beyond the existing patchers. (Its version:
  phase 0d, if needed. Table and connection configs: in scope.)
- Real replication. The store is a valid one-member replica set; there are
  still no other members, elections or cross-node oplog.
- The Python servers, which are deprecated.

## Appendix: phase 0 results (2026-11-19)

### 0a. WiredTiger compatibility: no upgrade needed

Measured with a `wt` utility built from the vendored, patched WiredTiger
(11.2.0, `mongodb-7.0.33`) with snappy, zstd, zlib and lz4 built in, against
mongod 8.2.11, which ships WiredTiger 12.0.0:

| Test | Result |
|---|---|
| WT 11.2 opens a clean mongod 8.2.11 store, lists its 135 metadata entries, reads `_mdb_catalog` | works |
| WT 11.2 writes a new table into that store and closes; mongod 8.2.11 starts on it with `--replSet`, reads, `validate({full: true})` is valid, takes writes | works |
| mongod 8.2.11 killed with `SIGKILL` after 1,000 journaled inserts; WT 11.2 runs recovery on the store | works |
| A store **created** by today's WT build (`WT_STANDALONE_BUILD=ON`); mongod 8.2.11 starts on it | **refused**: `WiredTiger.wt is an unsupported WiredTiger source file version 2.1; this WiredTiger build only supports versions from 1.1 to 1.1` |
| The same, with WT 11.2 rebuilt `WT_STANDALONE_BUILD=OFF` | works: replica set initiated, collection and index created, `validate` valid, and WT 11.2 reopens it afterwards |
| A store created by today's build, opened by the `WT_STANDALONE_BUILD=OFF` build | refused with `EACCES`, the same version check |

So decision 9's upgrade (phase 0d) is **not needed**. What is needed:

- **Build WiredTiger as mongod builds it: `WT_STANDALONE_BUILD=OFF`.** The
  flag decides more than the file version. With it off, WiredTiger also:
  does not write fast-truncate page-deletion cells in the 2022 format
  (`fast_truncate_2022`); tracks an unclean shutdown during recovery; and
  skips the rollback-under-cache-pressure check for read-only transactions
  (SERVER-44870). All are mongod's own behaviour, so faithful, but the last
  two change behaviour under load and need the stress gate.
- **snappy (and zstd for buckets) built in.** mongod's catalog, collections
  and journal are snappy-compressed; our builds carry only lz4 and zlib.
- **The open config mongod uses** for the log: `path=journal`,
  `compressor=snappy`.

Not measured: whether mongod's WiredTiger accepts an lz4 table (not needed
once tables are created with snappy); stores larger than a few thousand
documents; a mongod store with live prepared transactions.

### 0b. The specification

`tools/probes/mongod_store_layout.py` runs a fixed scenario against mongod
8.2.11 and records the store. Its output is
`tests/fixtures/storage_model/mongod-8.2.11-layout.json` (85 tables, 29
catalog entries). The scenario covers: one document per BSON type under an
ascending and a descending index (the KeyString corpus, 38 keys); compound,
unique, multikey, partial, sparse and TTL indexes; hashed, 2dsphere, 2d,
text and wildcard indexes; a collection default collation and a collated
index; capped, clustered and time-series collections; pre- and post-images
with one update; a view; a drop and a rename; a role and a user; one
transaction. The target table above is the summary.

### 0c. The comparison tool

The same probe with `--secantusd <binary>` runs the scenario against
`secantusd-mdb` and records that store (`--wt` must then be the build that
wrote it). The baseline from 0.5.3-beta.198 is
`tests/fixtures/storage_model/secantusd-mdb-0.5.3-beta.198-layout.json`: 32
`secantus_*` tables, no catalog. Found on the way:

- `secantusd-mdb` has no `shutdown` command (CommandNotFound); `SIGTERM` is
  its only clean stop. Phase 7's test stops a server the way mongod's
  tooling does, with `shutdown`, so this goes in the backlog.
- Hashed and text indexes are refused by the Rust server today ("not
  supported by the Rust storage engine yet"), so phase 4 starts with those
  two already in the "refuse by name" column.

The per-phase layout tests compare a SecantusDB capture against the mongod
fixture through the label view the probe writes (idents replaced by
namespace and index name).
