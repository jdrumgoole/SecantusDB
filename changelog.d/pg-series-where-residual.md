### Rust PG server: a WHERE over `generate_series` runs per row when it does not lower to a filter

A query over `FROM generate_series(...) f(x)` whose WHERE could not become a
storage filter used to be refused. An expression over the column (`where
f.x + 1 > 0`, `where x % 2 = 0`) was 0A000 `AExpr is not supported yet`. A
correlated subquery there (`where exists (select 1 where f.x > 1)`, `where
(select f.x + 1) > 0`, `where x in (select f.x)`) was 42883, with the
server's internal name for the per-row subquery call leaked into the message.
PostgreSQL's Function Scan applies such a qual to each row the function
returns, and now this server does too. A series WHERE goes through the same
filter-or-residual split a table's does: the conjuncts that lower stay the
filter, and the rest run per row. An undefined column or a bad literal is
still reported as before.

#### Fixed

- `secantus-pgplan`: `series_where` lowers through `lower_where_or_residual`
  and `plan_series_select` carries the residual. The series column is typed
  `int8` for an `int8` series. Fixes RP-703
  (tests/backlog/test_rp_found_rp699.py::test_correlated_subquery_in_a_function_scan_where),
  with `a_where_over_a_series_that_does_not_lower_is_a_residual` in
  `secantus-pgplan`'s tests.
