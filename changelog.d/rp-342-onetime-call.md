### Rust PG server: EXPLAIN prints a Result's One-Time Filter that calls a function (RP-342)

A FROM-less SELECT whose WHERE calls a stable function now explains as
PostgreSQL 15.19 does: the qual is kept unfolded, deparsed as the
`Result`'s `One-Time Filter`, and its evaluation is charged to the startup
and total cost.

#### Fixed

- `explain select 1 where current_setting('work_mem') <> ''` printed a bare
  `Result (cost=0.00..0.01 ...)`; it now prints `Result
  (cost=0.01..0.01 rows=1 width=4)` with `One-Time Filter:
  (current_setting('work_mem'::text) <> ''::text)`.
  Fixes RP-342 (tests/backlog/test_rp_explain_costs.py::test_plan[a result's one-time filter of a call])
