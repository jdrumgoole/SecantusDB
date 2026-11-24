### A read-only transaction's commit logs mongod's no-op

`secantusd-mdb` now logs the no-op mongod 8.2 writes when it commits a
transaction that wrote nothing, `{op: "n", ns: "", o: {msg: "read-only
transaction with writeConcern { ... }"}}`, naming the commit's write
concern, so the oplog matches mongod's entry for entry.

#### Fixed

- A read-only transaction's commit wrote no oplog entry.
  Fixes RM-747 (tests/backlog/test_rm_storage_model.py::test_a_read_only_transaction_logs_its_majority_no_op)
