### Rust MongoDB server: a snapshot read at a point in time reads the data as it was then

`readConcern: {level: "snapshot", atClusterTime: T}` used to read the latest
data whatever T said, and a `start_session(snapshot=True)` session saw other
clients' writes between its reads: a silent wrong answer. The server now
reads at T from WiredTiger's history, as mongod does. That history needs
mongod's storage split, so `secantusd-rs` now creates its stores that way:
the oplog written to the WiredTiger log, the data tables not, every write
stamped with its oplog timestamp, and the data recovered after a crash by
replaying the oplog from the last stable checkpoint.

#### Fixed

- `find`, `aggregate` and `distinct` with a snapshot read concern outside a
  transaction read as of `atClusterTime`, or as of every write committed so
  far when none is given, and report that time; a cursor's `getMore`s read
  at it too. The catalog is read at it as well: a collection made later is
  not there (a `find` of it says nothing of when, as mongod's does), and a
  hint naming an index made later fails (2). An `atClusterTime` later than
  any time handed out, or a null one, is 72; one older than the five-minute
  history kept is 239. Fixes RM-210
  (`tests/backlog/test_rm_found_2026_10_20b.py::test_snapshot_read_at_cluster_time`,
  `::test_snapshot_session_keeps_its_snapshot`,
  `::test_snapshot_reads_at_a_time_match_mongod`).
- A transaction is logged as mongod logs it: one `applyOps` at its commit,
  after any write another client made while it ran, and its writes commit
  at that time, so a read at a time before the commit never sees them --
  and does not wait for an open transaction. Each statement used to be
  logged when it ran. Found and fixed in this change as RM-314
  (`tests/backlog/test_rm_found_2026_10_25.py::test_a_snapshot_read_does_not_wait_for_an_open_transaction`,
  `::test_a_transaction_is_one_entry_at_its_commit`).

#### Changed

- `secantusd-rs`, the embedded `secantus_mdb::Server` and the `RustServer`
  test handle create new stores with data tables that are not logged (the
  `--data-nonlogged` mode, now the default); `--data-logged` creates them
  logged. An existing store keeps the mode it was created with. A store
  with logged data tables -- created with `--data-logged`, or by the
  PostgreSQL server, whose default is unchanged -- keeps no history, and
  refuses a snapshot read at a past time with 246 SnapshotUnavailable
  rather than answer it as of now.
- A store whose data tables are not logged keeps the synchronous oplog even
  when `--oplog-async` is given (its writes carry their oplog timestamps,
  which the asynchronous drainer mints only after they commit), and says so
  on its log.
- Crash recovery replays each oplog entry at its own timestamp, and skips
  the entries the last checkpoint already holds by its stable timestamp.

The storage changes are in `secantus-storage`, which the PostgreSQL server
links too. A store the PostgreSQL server creates is unchanged; one it opens
after the MongoDB server created it stamps its writes as well, logging each
statement as it runs (its prepared transactions and read-committed rebases
rely on that).
