### A slow-query line says how its read chose its plan

The Rust MongoDB server's `Slow query` line now carries `fromMultiPlanner: true` for a read whose plan a trial chose and `fromPlanCache: true` for one that ran the plan cache's active plan, as mongod 8.2.11's does.

#### Fixed

- A contested `find` or `aggregate` logged no `fromMultiPlanner` / `fromPlanCache`, though the server noted both for `$queryStats`.

Fixes RM-599 (tests/backlog/test_rm_found_rm363.py::test_a_contested_read_says_it_was_multi_planned)
