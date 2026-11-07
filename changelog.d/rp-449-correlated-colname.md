### Rust PG server: a subquery's output column is named as written

A correlated subquery whose target was an expression, `select (select a +
1) from t`, came back under the column name `\x1fcorrelated`, the internal
per-row call it is evaluated through, where PostgreSQL 15.19 names it
`?column?`. A client reading columns by name saw the internal one. The same
happened in an INSERT's, UPDATE's or DELETE's RETURNING, and through a cast
(`(select a)::int`, which PostgreSQL names `a`). A cast over an uncorrelated
scalar subquery, `(select 1 + 1)::int`, took the type's name, `int4`, where
PostgreSQL's `FigureColname` keeps the subquery's own.

#### Fixed

- A select-list or RETURNING target is named from the expression as
  written, before its subqueries are replaced, whenever the replacement
  would name it differently, `?column?` included.
- A scalar, `EXISTS` or `ARRAY(...)` subquery names its column through a
  cast around it.
- Fixes RP-449 (tests/backlog/test_rp_correlated_subquery.py::test_expression_target_is_named_column,
  tests/backlog/test_rp_correlated_subquery.py::test_returning_subquery_is_named_column;
  crates/secantus-pgplan `a_subquery_target_is_named_as_written`).
- Filed RP-466: a subquery in a CREATE TABLE AS or CREATE MATERIALIZED VIEW
  select list is refused.
