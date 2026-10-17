### Rust MongoDB server: `$merge` takes a write round per document

#### Fixed

- **`$merge` takes the locks mongod 8.2.11 takes.** It takes the global IS
  twice, then a write round for each document as it writes, whatever
  `whenMatched` / `whenNotMatched` say, and two rounds more when the write
  creates the target. `whenMatched: "fail"` takes one round for a batch
  that inserts cleanly. The rounds are taken around the writes themselves:
  the running statement's lock manager is reachable from the stage
  (`lock_manager::write_rounds`). All 10 shapes measured match.

Fixes RM-185 in part (tests/backlog/test_rm_185_locks.py::test_lock_counts_of_merge,
tests/backlog/test_rm_185_locks.py::test_lock_counts_of_merge_modes).
