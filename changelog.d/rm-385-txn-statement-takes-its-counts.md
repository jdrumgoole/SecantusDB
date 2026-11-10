### Rust MongoDB server: a transaction's update and delete statements report their own storage counts

Inside a transaction, mongod 8.2 writes two `Slow query` lines for an
`update` or a `delete`: one for each statement and one for the command. The
statement's line and its profile entry carry the dirty bytes and updates it
added (`storage.data.txnBytesDirty`, `txnNumUpdates`), and the command's
line carries none, because mongod's statement takes its transaction's
storage statistics since they were last taken and leaves its command only
what came after. This server put the statement's counts on the command line
and none on the statement's line or profile entry. A transaction's statement
now takes its share of the transaction's WiredTiger statistics when it
closes, as mongod's does, so the counts land where mongod prints them.

#### Fixed

- The `Slow query` line and profile entry of an `update` or `delete`
  statement inside a transaction carry its `txnBytesDirty` and
  `txnNumUpdates`, and the command's line no longer does
  (`secantus_wt::Session::statistics_since_last_call`,
  `secantus_storage::take_transaction_statistics`). The Rust PostgreSQL
  server links the changed `secantus-storage` and `secantus-wt` crates; it
  reads no session statistic and changes in nothing.
- Fixes RM-385 (tests/backlog/test_rm_355_364.py::test_an_update_in_a_transaction_counts_no_dirty_bytes,
  and `crates/secantus-wt/tests/operation_stats.rs::a_statement_takes_its_share_of_a_transaction`).
  Filed in passing: RM-499, a transaction's insert counts about 70 bytes
  more `txnBytesDirty` per document than mongod's.
