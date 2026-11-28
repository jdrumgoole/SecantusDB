### A `$group` or `$count` after `$addFields`-like stages counts mongod's interrupt checks

An `aggregate` whose classic `$group` or `$count` closes stages that pass
one document on for each (`$addFields`, `$set`, `$unset`, `$project`,
`$replaceRoot`, `$replaceWith`) now prints mongod 8.2.11's
`numInterruptChecks` and global lock count on its `Slow query` line, and
on its `getMore`s' lines.

#### Fixed

- The pipeline's cursor stage loads its buffers and ticks the pipeline's
  interrupt checker as mongod's does for such a `$group`, which pulls every
  document as a limited `$sort` does (RM-360, in part;
  `tests/mongod_live/test_interrupt_checks.py::test_a_pipeline_line_counts_mongods_interrupt_checks`).
