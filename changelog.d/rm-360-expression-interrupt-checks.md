### Rust MongoDB server: an aggregate with a `$map`, `$filter` or `$reduce` counts mongod's interrupt checks

An aggregate's slow-query line now prints `numInterruptChecks` when its
expressions run `$map`, `$filter` or `$reduce`, and the count is mongod
8.2.11's.

#### Fixed

- mongod checks for interrupt after every tenth element of each `$map`,
  `$filter` or `$reduce` evaluation, and counts the check. The count starts
  again with each evaluation. This server ticked the pipeline's 128-call
  checker once per element, so it printed no count for such a pipeline. It
  now checks where mongod does. A kill or `maxTimeMS` is also read within
  ten elements. RM-360 stays open for its other shapes.
  Part of RM-360 (tests/mongod_live/test_interrupt_checks.py::test_a_pipeline_line_counts_mongods_interrupt_checks).
