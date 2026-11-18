### Rust MongoDB server: an aggregate with a `$limit` counts mongod's interrupt checks

The `Slow query` line of an aggregate whose `$limit` stays after the cursor
stage (after an `$unwind`, say) now prints mongod 8.2's `numInterruptChecks`,
on the aggregate and on each `getMore`. Before, it printed none.

#### Fixed

- A `$limit` moved into the plan (`[{$addFields: ...}, {$limit: 50}]`) now
  makes the cursor stage fill one buffer up to the limit, as mongod's does,
  instead of starting with a buffer of 32. Before, the line counted one
  interrupt check and one global IS lock too many.
- A `$limit` left as a stage stops pulling from its source once it has
  passed its count. The documents after it, and the calls that would have
  read them, are not counted.

Fixes RM-360 in part (tests/mongod_live/test_interrupt_checks.py::test_a_pipeline_line_counts_mongods_interrupt_checks)
