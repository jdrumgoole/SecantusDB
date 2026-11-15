### Rust MongoDB server: `executionStats` of a change stream counts what the read found

The `explain` of a change stream under `executionStats` now runs the read as
mongod 8.2 does: the oplog scan from the start time, the stream's filter, and
each later stage, so a stream started before three inserts counts three at
every stage instead of none.

#### Fixed

- `executionStats` / `allPlansExecution` of a `$changeStream` aggregate report
  the oplog entries examined and kept, and each stage's `nReturned`.
  Fixes RM-453 (tests/backlog/test_rm_found_2026_11_09_rm076.py::test_execution_stats_count_what_the_read_found)
