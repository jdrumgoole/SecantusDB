### Rust MongoDB server: `explain` returns a `$replaceRoot`'s run-time error

`explain` at `executionStats` of a pipeline with a `$replaceRoot` or
`$replaceWith` now runs it and returns the executor's error, as mongod 8.2
does, instead of an explain document counting 0 documents.

#### Fixed

- `{$replaceRoot: {newRoot: "$x.b"}}` over a document whose `x.b` is not a
  document is 40228 from `explain`, with no "Executor error during
  aggregate command" prefix (as for a `$setWindowFields`, RM-617).

Fixes RM-659 (tests/backlog/test_rm_found_2026_11_14_rm536.py::test_explain_reports_a_replace_roots_run_time_error)
