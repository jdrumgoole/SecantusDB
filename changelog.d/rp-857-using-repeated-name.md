### A USING list that names a column twice is refused

`select * from fs join fe using (a, a)` used to run and return the join's rows. PostgreSQL refuses a USING list that repeats a name before it looks the name up in either side, and the Rust PostgreSQL server now does the same: 42701 `column name "a" appears more than once in USING clause`, with no position.

#### Fixed

- Rust PG server: a repeated name in a JOIN's USING list raises 42701, as `transformFromClauseItem` does.
  Fixes RP-857 (tests/backlog/test_rp_subquery_scope.py::test_using_clause_repeats_a_column)
