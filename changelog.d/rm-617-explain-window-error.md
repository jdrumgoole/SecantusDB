### Rust MongoDB server: `explain` reports a failing `$setWindowFields`

`explain` with `executionStats` (or `allPlansExecution`) runs a pipeline
holding a `$setWindowFields` before it answers, and returns the
executor's error where a window function fails at run time, as mongod
8.2.11 does: a `$sum` of a division by zero (code 2), an `$integral` or a
`$derivative` of a null input (5423900, 5624903). It returned an
`explain` document instead.

#### Fixed

- Fixes RM-617 (tests/backlog/test_rm_found_2026_11_13_rm509.py::test_explain_reports_a_window_functions_error)
