### Rust MongoDB server: `explain`'s winning plan reports its work accounting

`executionStats` now gives every stage of the winning plan the counters
mongod 8.2.11 gives: `works`, `advanced`, `needTime`, `needYield`,
`saveState`, `restoreState` and `isEOF`, and an index scan's `seeks`,
`dupsTested` and `dupsDropped`. Before, a stage had only its `nReturned` and
examined counts.

#### Fixed

- A `find`, `distinct` or aggregate plan's `executionStages` carry each
  stage's work accounting, counted by working the plan to its end as mongod
  works the winner. Under an aggregate's `$cursor`, `saveState` and
  `restoreState` count the batches the cursor loaded.
- Fixes RM-434 (tests/backlog/test_rm_found_2026_11_08_rm342.py::test_explain_gives_each_stage_its_work_accounting, tests/backlog/test_rm_found_2026_11_08_rm310.py::test_a_distinct_scan_run_reports_its_work_counters)
- Filed RM-692: a `$cursor` that the pipeline stops reading still reports a plan run to its end.
