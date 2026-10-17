### Rust MongoDB server: `serverStatus.locks` counts what mongod's does

#### Fixed

- **Commands take the locks mongod 8.2.11 takes**, and so
  `serverStatus.locks` counts them as mongod counts them: 21 of the 21
  commands measured, where none did before.
  - A read takes only the global IS. mongod's reads are lock-free below
    it, and this server's took database and collection intents too.
  - Each write statement takes a round of IX on the replication state
    transition, global, database and collection locks: a batch of
    statements takes one round each, an insert that creates its
    collection takes three, and `findAndModify` takes two.
  - `create`, `drop` and `dropDatabase` take mongod's rounds, and a
    `drop` of a missing collection takes only the database round.
  - `hello`, `ping`, `serverStatus` and `killCursors` take nothing; they
    took the global IX.
  - A statement inside a transaction still keeps its collection lock to
    commit or abort, so a `drop` waits on it as before.
- **The lock compatibility table is mongod's**, which is symmetric: an IS
  request waits on a held X. It used to be granted regardless, which hid
  the reads taking more than mongod's do.
- **`serverStatus.locks` lists resources in mongod's order**:
  `ReplicationStateTransition`, `Global`, `Database`, `Collection`, ... It
  had been sorted by name.

Fixes RM-185 in part (tests/backlog/test_rm_185_locks.py::test_lock_counts_per_command).
