# Plan: move the on-disk layout to mongod's storage model

Status: **reviewed 2026-11-19; three questions open for Joe** (see "Open
questions"). No code has changed.

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
5. **Oplog: keep the 16 seq-keyed shards for now.** Revisit after the
   KeyString work, with a `do-perf` concurrency run to decide whether one
   `local.oplog.rs` table keyed by timestamp holds up. Open question 1 may
   force this earlier.
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

## Open questions (for Joe)

1. **How is mongod started on our store: standalone, or as a replica-set
   member?** Standalone is the smaller claim: mongod ignores tables that are
   not in its catalog, so the 16 oplog shards can stay (decision 5). Starting
   with `--replSet` needs a real `local.oplog.rs`, `local.system.replset`
   and the `local.replset.*` collections, and moves the oplog work into this
   plan. The store advertises itself as a replica-set primary today, so the
   answer also decides what `local` holds.
2. **If mongod 8.2 cannot open files written by the vendored WiredTiger
   (mongodb-7.0.33), which gives way?** Either the vendored WT is upgraded
   (a large change: three builds and every `cmake/patch_wt_*.py` patcher), or
   decision 7 is dropped and the layout only *mirrors* mongod's. Phase 0a
   measures this before anything else is sized.
3. **Is the reverse direction a goal: SecantusDB opens a store mongod
   wrote?** It is a stronger fidelity test (we must read mongod's KeyStrings,
   not only write our own) and it gives users a migration path that needs no
   dump.

## The target: mongod's model

To be confirmed in phase 0 by inspecting a real mongod 8.2.11 data directory
(`wt list` / `wt dump`). These are what is understood today, not yet measured.

| Part | mongod |
|---|---|
| Collection | One WT table per collection, named by an *ident* (`collection-<n>-<random>`). Key: RecordId (`q`). Value: the BSON document (`u`). |
| Clustered collection | Same, but the key is the RecordId as a byte string, derived from `_id`, and there is no separate `_id_` index. `system.buckets.*` collections are clustered. |
| Index | One WT table per index (`index-<n>-<random>`). Key: KeyString of the indexed values, with the RecordId appended for a non-unique index. Value: KeyString TypeBits (and the RecordId for some unique-index formats). |
| `_id` | An ordinary unique index table, `_id_`. |
| Catalog | `_mdb_catalog`: a WT table keyed by RecordId. Each value is a BSON document holding the namespace, the collection options, the index specs and the ident of each table. |
| Sizes | `sizeStorer`: ident to `{numRecords, dataSize}`. |
| Oplog | The collection `local.oplog.rs`, keyed by a RecordId derived from the oplog timestamp. |
| Drop | WT `drop` of the table. A table still in use goes on a drop-pending list (the ident reaper) and is dropped once the timestamp it was dropped at is older than the oldest reader. |
| Rename | A catalog-only change: the ident and its file stay. |
| Logging | In a replica set, user tables are not logged; they rely on checkpoints and the oplog. The oplog and catalog are logged. |
| Compression | Collections `block_compressor=snappy` by default; indexes use prefix compression. mongod's WT build is believed to carry snappy, zlib and zstd, and not lz4 (to be measured in phase 0a). |
| Server collections | `admin.system.version` (the FCV document), `admin.system.users` / `system.roles`, `local.startup_log`, `config.system.preimages`, `config.transactions` and others a fresh mongod creates. |

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
| 16 oplog shards keyed by seq (S:1672), `secantus_oplog_meta` (S:2108) | Keep for now (decision 5, open question 1) | oplog code |
| Tables `block_compressor=lz4` (S:2722, S:2726) | The compressor mongod uses (snappy by default); our CMake already builds snappy and zstd in | `DOC_TABLE_CFG`, `QU_COMPRESSED_CFG`; needs a `do-perf` run |
| User tables logged; connection `log=(enabled=true)` (S:2673); a non-logged mode exists (S:1676-1716) | To be decided in phase 0 from what mongod accepts in the mode open question 1 picks | table-create configs |
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

What does not change: the BSON-blob principle, the oplog-backed change
streams, the per-collection RecordId counter, and the PostgreSQL server's own
catalog collections (`__sql_catalog__` and the rest stay ordinary
collections). The vendored WiredTiger version does not change unless open
question 2 says so. Table creation configs **do** change (compressor,
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

Phases 2 to 5 each change the format. Each one **bumps the store-format
version**, so a store written by an earlier phase is refused by name like any
other old store. **No release of either Rust server is cut between phase 2
landing and phase 6 landing**, so no user ever holds an intermediate format.

### The phases

0. **Measure the target.**
   - **0a. WiredTiger compatibility, first.** Write a store with the vendored
     WT 7.0.33 holding mongod-shaped tables, and start mongod 8.2.11 on it.
     Record which compressors mongod's WT accepts, what FCV document it
     demands, and whether it then rewrites files so that WT 7.0.33 can no
     longer open them. If mongod refuses the files, stop and take open
     question 2 to Joe before any other work.
   - **0b. The specification.** Create a mongod 8.2.11 store (in the mode
     open question 1 picks): a collection, a compound index, a unique index,
     a TTL index, one index of each kind listed under phase 4, a capped
     collection, a clustered collection, a time-series collection, a user and
     a role, a collection with pre-images, then a drop and a rename. Record
     its tables, their creation configs, key and value formats, catalog
     documents, size-storer rows, `storage.bson`, and every collection in
     `local`, `admin` and `config`.
   - **0c. The comparison tool.** A probe that runs one script of operations
     against mongod and against SecantusDB and diffs `wt list`, the table
     configs and the catalog documents.
   - Output: an appendix here, the probes saved in `tools/probes/`, and the
     logging decision recorded in the "what changes" table.
1. **Store format version and refusal.** Add one store-format version, written
   when a store is created. A build refuses an older store by name. The
   message names the last release that reads the old layout and gives the
   dump-and-restore commands for each server (`mongodump` / `mongorestore`;
   `pg_dump` / `psql`). All existing `upgrade_*` paths are removed. This
   lands with phase 2, not before, or `main` would refuse stores it still
   writes.

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
5. **Timeseries buckets.** Store time-series data as `system.buckets`
   bucket documents in a clustered collection, as mongod does; remove the
   framed value.
6. **Size storer and the PostgreSQL server.** Key sizes by ident.
   `pg_relation_filepath` (secantus-pg/src/lib.rs:22636) returns the ident's
   file. Confirm the SQL-to-collection hand-off test still passes.
7. **mongod opens the store.** A test creates a store with SecantusDB holding
   everything phase 0b created, stops it cleanly, starts mongod 8.2.11 on the
   directory in the mode open question 1 picks, and runs
   `validate({full: true})` on every collection, then reads every document
   and walks every index. The test also states the limits of the claim (for
   example: standalone only, oplog not carried). If open question 3 is yes,
   the reverse test lands here too.
8. **Clean-up.** Remove the legacy-table readers, the shard routing and the
   format markers this replaces; rewrite the layout tests below. Update
   CLAUDE.md's "One on-disk format" section, the `python-servers` skill's
   layout description, and the docs pages that describe the store.

## Ownership and parallel work

- This change spans both Rust servers, which CLAUDE.md's "one session works on
  ONE Rust server" rule otherwise forbids. **One session owns the whole plan,
  on the MongoDB side**, and makes the shared-storage and `secantus-pg`
  changes itself; each PR says the other server links the change.
- **No `fix_many` lane runs while a phase 3 or phase 4 branch is open.** Those
  phases rewrite most of a 31,000-line file the lanes also edit. Phases 2, 5
  and 6 are narrower and can run beside one lane.
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
  upgrade them past what 7.0.33 reads. Phase 0a; open question 2.
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
- **Logging mode.** If mongod only accepts non-logged user tables in the
  chosen mode, durability moves to checkpoints plus the oplog, which is a
  recovery redesign, not a config flag. Phase 0 must settle this before
  phase 3 is sized.
- **Index kinds.** Matching ICU, S2 and the text format exactly may be out of
  reach for some kinds; those are refused by name, not approximated.
- **Backup and PITR archives** name tables; `pitr_archive.rs`, `repair.rs` and
  `compact.rs` need the new names.

## Not in scope

- Changing WiredTiger's code. (Its version: open question 2. Table and
  connection configs: in scope, see "What changes".)
- Moving the oplog to `local.oplog.rs`, unless open question 1 requires it.
- The Python servers, which are deprecated.
