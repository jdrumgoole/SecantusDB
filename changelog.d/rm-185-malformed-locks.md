### Rust MongoDB server: a malformed `dropIndexes`, `dbStats` or `listDatabases` takes no lock

mongod parses a command before it takes the command's locks, so a request
it refuses as malformed takes none. `dropIndexes` without `index`, and
`dbStats` and `listDatabases` with a wrongly typed or unknown field, now
parse before their locks here too, and their `serverStatus.locks` deltas
match mongod 8.2.11's (none). The other malformed commands of RM-185 were
closed by RM-386.

#### Fixed

- `dropIndexes`, `dbStats` and `listDatabases` parse in
  `parse_before_locks`, before authorization, admission and locks.
- The lock comparison's index-build check no longer requires a build poll of
  a `createIndexes` that mongod refuses before any build.

Fixes RM-185 (tests/backlog/test_rm_185_locks.py::test_lock_counts_of_other_malformed_commands)
