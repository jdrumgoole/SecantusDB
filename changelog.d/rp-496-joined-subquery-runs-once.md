### A volatile function in a joined FROM subquery or CTE runs once

A function with a side effect, called in a FROM subquery or a CTE that is
joined to a table, ran twice on the Rust PostgreSQL server: `select * from
t, (select f()) s` and `with w as (select f()) select * from t, w` each
wrote `f`'s insert two times, where PostgreSQL 15.19 writes it once.

#### Fixed

- The narrow two-table join path planned the subquery side -- running its
  calls -- before refusing the shape, and the general join planner then
  planned it again. When the FROM runs a volatile call, the narrow attempt is
  now made with calls switched off, and made for real only when it will
  stand, as an aggregate's first attempt already was (RP-476).

Fixes RP-496 (tests/backlog/test_rp_from_function_scan.py::test_joined_subquery_runs_its_function_once)
