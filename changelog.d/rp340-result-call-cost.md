### Rust PG server: EXPLAIN prices a Result whose select list calls a function

`explain select current_setting('work_mem')` printed `Result
(cost=0.00..0.00 rows=0 width=0)`; it now prints PostgreSQL 15's
`Result  (cost=0.00..0.01 rows=1 width=32)`.

#### Fixed

- A select with no FROM whose select list keeps a call (`current_setting`,
  `set_config`, `nextval`, `currval`, `setval`, `pg_backend_pid`,
  `pg_sleep`, `pg_notify`, `session_user`, `current_user`, ...) adds each
  call's `cost_qual_eval` -- its `procost` times `cpu_operator_cost`, plus
  its arguments' -- to `cost_resultscan`'s row, and prints the return
  type's width. Fixes RP-340
  (tests/backlog/test_rp_explain_costs.py::test_plan[a result of a function call]).
