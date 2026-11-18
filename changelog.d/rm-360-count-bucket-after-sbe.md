### Rust MongoDB server: a `$count`, `$sortByCount` or `$bucket` counts mongod's interrupt checks

An `aggregate` ending in a `$count`, `$sortByCount` or `$bucket` that
mongod's slot-based engine runs now prints mongod 8.2.11's
`numInterruptChecks` and cursor-stage locks on its `Slow query` line, as a
`$sort` after a `$group` already did. A `$switch` of 50 branches or more,
and so a `$bucket` of 50 buckets or more, keeps its `$group` in the classic
engine, as on mongod.

#### Fixed

- `[{$count: "n"}]` printed no `numInterruptChecks` where mongod prints 5;
  a `$sortByCount` of 300 groups none where mongod prints 12 and its
  `getMore` 3.
- A `$bucket` of 50 or more buckets was taken as a slot-based `$group`.

Fixes RM-360 in part (tests/mongod_live/test_interrupt_checks.py::test_a_pipeline_line_counts_mongods_interrupt_checks)
