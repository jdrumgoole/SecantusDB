### An insert of more than 50 documents reports mongod's read round

Before it inserts more than a tenth of `internalInsertMaxBatchSize` (500)
documents, mongod 8.2 reads the collection's index count to size its
batches, in a read round of its own. The Rust MongoDB server's slow-query
line and profile entry now count that round: the transition lock, and the
global, database and collection locks in IS, once per command and outside a
transaction.

#### Fixed

- The `locks` of an `insert` of more than 50 documents match mongod 8.2.11,
  ordered or not, retryable or not, failing or not. This closes part of
  RM-357 (tests/backlog/test_rm_355_364.py::test_a_write_reports_mongods_locks_and_flow_control).
