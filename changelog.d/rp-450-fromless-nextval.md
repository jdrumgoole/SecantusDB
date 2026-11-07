### Rust PG server: a FROM-less correlated subquery draws its sequence once per row

`select (select nextval('q') + a) from (values (1), (2)) t(a)` answers 2
and 4 and leaves the next draw at 3, as on PostgreSQL 15.19. It answered 3
and 5 and skipped a sequence value no later statement could get back.

#### Fixed

- The plan the server makes of a correlated subquery's body only to check
  and type it no longer calls a function or draws a sequence. A FROM-less
  body's targets are evaluated as it is planned, so that plan drew one
  `nextval` before the per-row runs began, at one level of nesting or two.

Fixes RP-450 (tests/backlog/test_rp_correlated_subquery.py::test_fromless_correlated_nextval_draws_once_per_row)

#### Filed

- RP-452: a subquery with more than one column (`select (select 1, 2)`,
  `1 in (select 1, 2)`) runs on its first column, where PostgreSQL refuses
  it with 42601 (tests/backlog/test_rp_subquery_columns.py::test_subquery_with_more_than_one_column).
