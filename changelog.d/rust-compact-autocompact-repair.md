### Rust MongoDB server: `compact` gives disk space back, `autoCompact` runs it in the background, and `--repair` salvages a damaged store

`compact` used to answer `bytesFreed: 0` without doing anything, so a store
never got back the space of deleted documents or dropped collections, and
`autoCompact` was 59 CommandNotFound. Both now run WiredTiger's compaction, as
mongod 8.2.11 does, and `secantusd-rs --repair` does what `mongod --repair`
does: verify every table, salvage one that fails, rebuild the indexes of the
collections it held, and exit. The Rust PostgreSQL server links the same
storage crate and gains nothing it uses yet.

#### Added

- `secantus-wt`: bindings for `WT_SESSION::compact`, `verify` and `salvage`,
  and `WtError::is_try_salvage` / `is_busy`.
- `compact` compacts the tables the collection is kept in: its document shard,
  the `_id` table, and the entries and claims tables when it has indexes that
  use them. Collections share those files, so the space given back is the
  free space in them, whoever freed it, and `bytesFreed` is how much the files
  shrank (mongod's measure, clamped at 0, an int32 when it fits). Measured
  against mongod 8.2.11: 20,000 documents of 2 KB with 90% deleted give back
  about 36 MB on both, nothing above `freeSpaceTargetMB` (default 20 MB), and
  little on a second run.
- `compact`'s options and errors as mongod's: `force` / `dryRun` booleans and
  `freeSpaceTargetMB` (a whole number of at least 1, a double truncated) parsed
  in the order sent, `estimatedBytesFreed` for a dry run, a time-series
  collection through either name, `local.oplog.rs` on a replica-set member,
  `system.views` when the database has a view, the namespace checked before
  the other fields, and a second `compact` of the same collection while one
  runs refused (96, "Compaction is already in progress").
- `autoCompact`: WiredTiger 11.3's background compaction server rebuilt over
  the vendored 11.2 -- every file in metadata order, the oplog excluded, files
  of 1 MB or less and recent poor results skipped, `runOnce`, a 10-second rest
  after each pass, a pause under cache pressure. Enabling it with another
  configuration while it runs is 23 AlreadyInitialized, and a change sent
  before the server has taken up the last one is 314 ObjectIsBusy; it answers only on
  `admin`; its twelve `background-compact` counters are in
  `serverStatus.wiredTiger` where mongod lists them.
- `secantusd-rs --repair`: verify and salvage on a bare connection before the
  store opens (with `salvage=true` when WiredTiger's metadata is damaged),
  then each collection checked against its indexes and rebuilt where a
  salvage touched it or they disagree. Exits 0, or 100 (mongod's code) when it
  cannot finish, a missing storage path included.

#### Fixed

- Fixes RM-192 (`tests/backlog/test_rm_192_compact.py::test_compact_answers_as_mongod`,
  `::test_compact_gives_a_mostly_deleted_collections_space_back`,
  `::test_repair_exits_as_mongod_does`,
  `::test_repair_salvages_a_damaged_collection`; Rust:
  `crates/secantus-storage/tests/compact.rs`, `crates/secantus-storage/tests/repair.rs`,
  `crates/secantus-wt/tests/compact.rs`).
- Fixes RM-219's `autoCompact` part
  (`tests/backlog/test_rm_192_compact.py::test_auto_compact_answers_as_mongod`,
  `::test_auto_compact_reports_itself_in_server_status`); its other three
  commands stay open.

#### Filed

- RM-316 (a dry run does not estimate; mongod answered 0 on every shape
  measured), RM-317 (background compaction cannot interrupt a file and weighs
  files by bytes given back), RM-318 (`--repair` keeps a resurrected duplicate
  `_id` where mongod moves it to `local.lost_and_found`), RM-319 (there is no
  `local.startup_log`).
