### A transaction statement's log line prints no locks

The Rust MongoDB server's slow-operation line for a statement of a
multi-document transaction now prints `locks: {}`, as mongod 8.2 does: the
statement runs on the transaction's stashed locker.

#### Fixed

- A statement inside a transaction (`autocommit: false`) logged the global,
  database and collection IX it took.
  Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_a_transaction_statement_line_prints_no_locks)
