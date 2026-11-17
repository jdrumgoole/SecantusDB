### A refused write of a view counts mongod's locks

The Rust MongoDB server now refuses an `insert`, `update` or `delete` of a
view where mongod 8.2.11 does: after a retryable write has checked its
session out, and after an insert's two write rounds. Its slow-query lines
and profile entries for the refusal count the same locks as mongod's.

#### Fixed

- A retryable `update` / `delete` of a view was refused before its session
  was checked out, so its lines were one `ReplicationStateTransition` lock
  short; an `insert` into a view counted none of its write rounds and no
  flow control ticket. A statement of a transaction is now refused under
  the transaction's locks; what those count is RM-715.

Fixes RM-589 (tests/backlog/test_rm_view_namespaces.py::test_a_refused_write_of_a_view_takes_mongods_locks)
