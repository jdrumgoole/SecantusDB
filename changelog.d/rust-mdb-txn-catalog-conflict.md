### A transaction cannot write past an index created after it began

A transaction whose snapshot was taken before an index was created could still write to that index's collection, and its write used the index list from its own snapshot. The document committed with no entry in the new index, so a query through the index missed it. The Rust MongoDB server now follows mongod's rule: once a collection's catalog entry has changed after a transaction's snapshot, a write by that transaction to the collection fails before it runs. The error is 112 WriteConflict "Unable to write to collection '<ns>' due to catalog changes; please retry the operation", labelled TransientTransactionError, and the transaction is aborted. A driver's `with_transaction` retries it on a fresh snapshot.

The rule covers every catalog change mongod 8.2.11 counts: an index build starting, committing or aborting, a dropped index, `collMod`, a drop, `dropDatabase`, and a rename on both the source and a replaced target. It covers every write command in a transaction, including one that would match nothing and each op of a `bulkWrite`. Reads are not affected, and neither is a write to a collection whose catalog did not change. Creating an index that already exists changes nothing.

#### Fixed

- `secantus-storage` (`catalog_changes`): a per-collection mark of the last catalog change, and the counter value each user transaction notes before its snapshot (`UserTransactionHandle::catalog_changed_since_snapshot`). The Rust PostgreSQL server links this crate but never marks a change, so its behaviour is unchanged.
- `secantus-storage-adapter`: `createIndexes`, the start, commit and abort of a hybrid build, `dropIndexes`, `collMod` (options and index options), `drop`, `dropDatabase` and `renameCollection` mark the collections whose catalog entry they change.
- `secantus-commands` (`session`): an `insert`, `update`, `delete`, `findAndModify` or `bulkWrite` in a transaction is refused with mongod's 112 when a collection it writes changed after the transaction's snapshot. A `bulkWrite` names the first collection in op order, as mongod does. A transaction older than a dropped collection used to fail its insert there with an internal WiredTiger duplicate-key error; it now gets the same 112.
- Fixes RM-320 (tests/backlog/test_rm_index_build_gaps.py::test_a_transaction_older_than_an_index_cannot_write_past_it)
- Regression tests: `tests/mongod_live/test_txn_catalog_changes.py` (each catalog change and write command against mongod 8.2), and `catalog_changes::tests` in `secantus-storage`.
