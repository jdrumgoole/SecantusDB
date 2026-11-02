### Rust PG server: EXPLAIN prices a select with no FROM

`explain select 1` now prints PostgreSQL 15's `Result  (cost=0.00..0.01
rows=1 width=4)` instead of zeros: one row at `cpu_tuple_cost`, its width
the select list's types' average widths (`get_typavgwidth`, a bounded
`varchar` included).

#### Fixed

- A `Result` over a select list of constants carries PostgreSQL's cost,
  rows and width. A select list holding a call the planner keeps
  (`current_setting`, `nextval`, ...) still prints zeros (RP-340).

Fixes RP-325 (tests/backlog/test_rp_explain_costs.py::test_plan[result])
