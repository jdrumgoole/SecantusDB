### Rust MongoDB server: index builds, `$out` and `dbStats` take mongod's locks

#### Fixed

- **`createIndexes`, `dropIndexes`, `$out` and `dbStats` take the locks
  mongod 8.2.11 takes**, so `serverStatus.locks` counts them as mongod's.
  All 15 measured shapes match.
  - **An index build:** the global IS twice, the state transition lock
    alone twice, the collection in X three times and in IX five times as
    the build moves through its phases. On an empty collection it is once
    each.
  - **`createIndexes` variants:** a write round when every index is there
    already, and two write rounds when it creates the collection.
  - **`$out`:** a round reading the source, seven global IS, the target in
    X twice (three times when it replaces an existing one) and two write
    rounds.
  - **`dbStats`:** IS on each of the database's collections and views.

Fixes RM-185 in part (tests/backlog/test_rm_185_locks.py::test_lock_counts_of_index_builds_out_and_db_stats,
tests/backlog/test_rm_185_locks.py::test_lock_counts_of_index_build_and_out_shapes).
