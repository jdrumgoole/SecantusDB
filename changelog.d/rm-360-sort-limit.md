### Rust MongoDB server: a $sort with a $limit after $addFields-like stages counts mongod's interrupt checks

An `aggregate` that closes with a `$sort` and a `$limit` after stages passing
one document on for each now prints `numInterruptChecks` on its line and its
`getMore`s' lines, as mongod 8.2.11 does.

#### Fixed

- The `$limit` folds into the `$sort` as on mongod (a top-k sort), which
  still pulls every document through the cursor stage and the stages before
  it: the buffers and ticks are counted from the documents the `$sort` took
  in, not the few it passed on. Part of RM-360
  (tests/mongod_live/test_interrupt_checks.py::test_a_pipeline_line_counts_mongods_interrupt_checks).
