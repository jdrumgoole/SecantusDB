### Rust PG server: a USING clause's name errors carry no position

A JOIN's USING column that is missing from one side (42703), or that names
two of a side's columns (42702), is reported with no position, as
PostgreSQL 15.19 reports it: `transformFromClauseItem` matches the names
without a `parser_errposition`. The Rust server had pointed into the
statement at the first token spelling the name.

#### Fixed

- `select * from fs join fe using (nosuch)` and
  `select * from fs as x(p) join fe using (a)` report their 42703 with no
  position.

Fixes RP-481 (tests/backlog/test_rp_subquery_scope.py::test_using_column_missing_has_no_position)
