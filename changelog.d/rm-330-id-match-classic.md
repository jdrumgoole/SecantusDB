### A pipeline that reads one `_id` runs in mongod's classic engine

mongod 8.2 keeps a whole aggregation in its classic engine when the query at the front of the pipeline is an `_id` point read. That holds once the planner has optimised the filter: `{_id: 1}`, `{_id: {$in: [1]}}`, `{_id: {$in: [1, 1.0]}}` and `{$or: [{_id: 1}]}` all qualify. The read must also have no hint, no `$skip` and the collection's own collation. The Rust MongoDB server still pushed a following `$group` or `$lookup` into the slot-based engine, so an expression that fails differently in the two engines gave the wrong error. One example is `$mod` by zero, which was 4848403 here and 16610 on mongod. The engine choice now follows the same rule as mongod, wherever the server makes it: running the pipeline, streaming its cursor, a `$unionWith` sub-pipeline, the plan-cache key, and the `queryFramework` that the slow-query log and the profiler report.

#### Fixed

- Fixes RM-330 (tests/backlog/test_rm_222_invalid_utf8.py::test_a_group_after_an_id_match_runs_in_the_classic_engine); the other shapes mongod 8.2.11 was probed with are pinned by tests/backlog/test_rm_222_invalid_utf8.py::test_only_an_id_point_read_keeps_the_group_classic.
- `explain`'s `parsedQuery` now treats `$in` values that compare equal as one value. `{$in: [1, 1.0]}` becomes `{$eq: 1}`, as on mongod, where before it stayed `{$in: [1]}`.
