### `explain` lists the DISTINCT_SCANs a `$group` tried

When an aggregate's `$group` can be answered from a DISTINCT_SCAN and the pipeline sorts or filters first, mongod tries every index that could serve it against the others and lists the losers in `rejectedPlans`. The Rust MongoDB server picked the same index but listed none, and `allPlansExecution` held nothing. It now runs mongod's trial over the same candidates: each serving index as a DISTINCT_SCAN in the form the `$group` reads it (covered, or fetching), beside the plans the filter gives that are no DISTINCT_SCAN, in the catalog's order. The scores are mongod 8.2's, including the `groupByDistinctBonus` its plan ranker gives a DISTINCT_SCAN in an aggregate, and a DISTINCT_SCAN that fetches now counts as a fetch when it is scored. Unsorted and unfiltered, mongod tries nothing, and neither does this server.

#### Fixed

- `explain` of `[{$sort: {x: 1}}, {$group: {_id: "$x", f: {$first: "$y"}}}]` (or a `$match` on `x`) over `x_1`, `x_1_y_1` and `x_1_z_1` lists `x_1` and `x_1_z_1` in `rejectedPlans`, and `allPlansExecution` gives the trial's scores (`3.0003`, `3.0002`) and counters, with the winner saved once as the trial ends, as on mongod 8.2.11.
- A fetching DISTINCT_SCAN loses the no-fetch tie-breaker in any trial, as mongod's `doesFetch` has it.
- Fixes RM-310 (tests/backlog/test_rm_found_2026_10_25.py::test_explain_lists_the_distinct_scans_a_group_tried, tests/backlog/test_rm_found_2026_10_25.py::test_a_distinct_scan_groups_trial_scores_as_mongods).

#### Added

- Backlog RM-446 (a DISTINCT_SCAN plan's `executionStats` gives no work counters) and RM-447 (a DISTINCT_SCAN bounded on a trailing field examines fewer keys than mongod's), each with a gap test in `tests/backlog/test_rm_found_2026_11_08_rm310.py`. Backlog RM-448: the profiler's level-two adapter test fails about once in 31 runs when its `find` entry carries a `storage` section.
