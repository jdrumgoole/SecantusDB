### Rust MongoDB server: a transaction statement counts its updates

mongod 8.2's `Slow query` line and profile entry for a statement inside a
transaction say how many updates it added (`storage.data.txnNumUpdates`)
beside the dirty bytes it added (`txnBytesDirty`). The count is a WiredTiger
11 session statistic, `txn_updates`, which the WiredTiger this server builds
on (mongodb-7.0.33) did not keep. A new source patch ports it: WiredTiger
counts each new insert, append or update a running transaction links into a
page, under the same conditions as its dirty bytes, and the line now carries
the count. An insert of one document over `_id` and one more index counts 4
updates, three documents 12, as mongod does.

#### Fixed

- `cmake/patch_wt_txn_updates.py` adds WiredTiger 11's `txn_updates`
  (`session: number of updates in this txn`, `WT_STAT_SESSION_TXN_UPDATES`)
  to the vendored WiredTiger, applied by the wheel's CMake build, the
  `secantus-wiredtiger-sys` bundled copy (`wiredtiger.sha256` refreshed) and
  `./inv rust-wt-build`. The Rust PostgreSQL server links the same WiredTiger
  build; it reads no session statistic and changes in nothing.
- The `Slow query` line and profile entry of a statement in a transaction
  carry `storage.data.txnNumUpdates`.
- Fixes RM-364 in part (tests/backlog/test_rm_355_364.py::test_a_transaction_statement_counts_its_updates,
  and `crates/secantus-wt/tests/operation_stats.rs::a_transaction_counts_its_updates`):
  `txnNumUpdates`. Its other half, `cacheInterruptibleMicros`, stays open.
  Filed in passing: RM-385, inside a transaction mongod's `update` and
  `delete` lines carry no `txnBytesDirty` or `txnNumUpdates`, and this
  server's do.
