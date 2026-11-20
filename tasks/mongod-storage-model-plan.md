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
5. **Timeseries buckets.** Store time-series data as `system.buckets`
   bucket documents in a clustered collection, as mongod does; remove the
   framed value.
6. **Size storer and the PostgreSQL server.** Key sizes by ident.
   `pg_relation_filepath` (secantus-pg/src/lib.rs:22636) returns the ident's
   file. Confirm the SQL-to-collection hand-off test still passes.
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
