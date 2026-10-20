### Rust MongoDB server: two transactions may create one collection, and the clash comes at commit

#### Fixed

- A collection created in a transaction no longer holds the collection's
  exclusive lock until that transaction ends. A second transaction may now
  create, insert into or read the name meanwhile, and whichever commits
  second gets 112 WriteConflict ("Collection namespace ... is already in
  use"), labelled TransientTransactionError, as on mongod 8.2.11. Before,
  the second transaction's statement failed with 24 LockTimeout. The create
  holds the collection in IX, which still holds a concurrent `drop` off
  (RM-084). All eight probed shapes match mongod: create, insert, find and
  count, with either transaction committing first.
- A lock timeout on the global lock names it as mongod does,
  `'{Global : 0}'`.

Fixes RM-202 (`tests/backlog/test_rm_057_084.py::test_a_collection_made_in_two_transactions`).
The RM-201 tests in `tests/backlog/test_rm_transaction_locks.py` now
provoke a lock wait the way mongod really does: a second transaction
queued behind a `drop` that waits for the first. They had used two creates,
which mongod does not make wait.
