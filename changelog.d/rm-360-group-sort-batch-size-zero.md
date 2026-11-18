### Rust MongoDB server: a `$sort` after an SBE `$group` asked for no first batch counts its interrupt checks

An `aggregate` of a `$group` mongod's slot-based engine runs and a `$sort`
after it, asked for `batchSize: 0`, now prints mongod 8.2.11's
`numInterruptChecks` (5) on its line, where it printed none, and its
`getMore` takes no lock of its own -- the cursor stage takes a global IS
per buffer it loads -- so its line counts mongod's checks and locks
(4 under one global IS for three groups, where it counted 5 under two).

#### Fixed

- The `aggregate` and `getMore` lines of a `batchSize: 0` `$group` +
  `$sort` pipeline carry mongod's `numInterruptChecks` and `locks`.

Fixes RM-360, in part (tests/mongod_live/test_interrupt_checks.py::test_a_pipeline_line_counts_mongods_interrupt_checks)
