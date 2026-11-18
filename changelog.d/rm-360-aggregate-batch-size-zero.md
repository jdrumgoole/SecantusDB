### Rust MongoDB server: an `aggregate` asked for no first batch counts its interrupt checks

An `aggregate` with `batchSize: 0` plans its pipeline and reads nothing
until the first `getMore`, as on mongod 8.2.11, and its `Slow query` line
now prints mongod's `numInterruptChecks` for it: 5, one more per index
scan, as a `find` of no batch. It printed none.

#### Fixed

- The line of a `batchSize: 0` `aggregate` whose pipeline mongod's query
  layer runs whole, or streams through its cursor stage, carries
  `numInterruptChecks`. A `$sort` after an SBE `$group` asked for no batch
  still prints none (RM-360 keeps it).

Fixes RM-360, in part (tests/mongod_live/test_interrupt_checks.py::test_a_pipeline_line_counts_mongods_interrupt_checks)
