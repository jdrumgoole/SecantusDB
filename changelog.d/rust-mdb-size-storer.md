### Rust MongoDB server: `count`, `collStats` and `dbStats` answer from a size storer, without reading the collection

`count` with no query, `collStats`, `$collStats`, `dbStats` and an estimated
`dataSize` each read every document of the collection, so their time grew
with it. mongod answers them from per-collection metadata, its size storer,
and the storage layer now keeps the same thing: each collection's record
count, data size and index sizes. Every write that adds, changes or removes
a document row or an index entry moves the numbers at once, inside its
WiredTiger transaction, and a rollback moves them back
(`secantus_wt::Session::on_transaction_end`, the counterpart of mongod's
`RecoveryUnit::onRollback`). On a debug build, `count` on 200,000 documents
took 0.21 ms against 0.25 ms on 2,000; a `countDocuments({})`, which still
reads them, took 865 ms against 10 ms.

The storage crate is shared: the Rust PostgreSQL server links it and keeps
the same numbers, so a store handed from one server to the other carries
them.

#### Changed

- `count` with no query and no hint, `collStats`, `$collStats` and `dbStats`
  read the kept numbers. Like mongod's they include another transaction's
  uncommitted writes: with 2 committed documents and an open transaction's
  insert, `count` answers 3, as mongod 8.2.11 does, and 2 again after the
  abort.
- `dataSize` follows mongod 8.2.11: with `estimate: true` and no bounds it
  answers the kept numbers; with bounds and an estimate, their average
  document size for each document counted; without an estimate it reads the
  collection as before. A collection whose record count is 0, or that does
  not exist, answers zeros with no `estimate` field before looking for an
  index for the bounds. On a time-series collection it refuses with 166, as
  mongod refuses it on a view.
- The numbers are saved at a clean close (`table:secantus_size_storer`,
  beside a clean-close marker and the oplog state they go with) and read
  back at open. A store that did not close cleanly, or was written by a
  build that keeps no numbers, is counted by one walk of its tables at open:
  the answer a mongod replica-set member's recovery reaches (1,300 documents
  written and the process killed, 1,300 counted after the restart, on both).
- A foreground `validate` that finds the collection sound sets its numbers
  to what it read, as mongod's does.
- `SECANTUS_CHECK_SIZES=1` makes every close compare the kept numbers with a
  walk of the store and fail the close when they differ: a sweep for a write
  path that moves a row without counting it.

#### Fixed

- Fixes RM-191 (`crates/secantus-storage/tests/size_storer.rs`,
  `crates/secantus-storage-adapter/tests/size_storer_wt.rs::the_size_commands_read_no_document`).
- Fixes the fast-count part of RM-087
  (`tests/backlog/test_rm_085_112.py::test_fast_count_sees_another_sessions_uncommitted_insert`).
