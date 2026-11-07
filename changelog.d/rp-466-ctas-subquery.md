### Rust PG server: a subquery in CREATE TABLE AS or CREATE MATERIALIZED VIEW

`create table t as select (select 1) x from e` was refused with `0A000
SubLink is not supported yet`, uncorrelated or correlated, and so was every
`CREATE MATERIALIZED VIEW` whose list held a subquery, while `SELECT ... INTO`
of the same list was made. The CREATE's query now goes through the same
analysis as a bare SELECT, as PostgreSQL's `transformCreateTableAsStmt` hands
it to `transformStmt`, so its subqueries, views and CTEs resolve as a SELECT's
do. A materialized view keeps its query as written, so `REFRESH` runs its
subqueries again over the rows there now.

#### Fixed

- `CREATE TABLE AS` and `CREATE MATERIALIZED VIEW` take a scalar, `EXISTS`
  or `IN` subquery in their select list or WHERE, uncorrelated or
  correlated.
- A `CREATE TABLE AS` / `CREATE MATERIALIZED VIEW` that fills nothing (`WITH
  NO DATA`, or a relation of that name already there, which `IF NOT EXISTS`
  skips and anything else refuses with 42P07) analyses its query but no
  longer runs it. Its subqueries draw no sequence value and call no
  function, as on PostgreSQL 15.19. `SELECT ... INTO` a relation already
  there is the same: before, it drew the value and then failed with 42P07.

Fixes RP-466 (tests/backlog/test_rp_ctas_subquery.py::test_subquery_in_create_table_as,
tests/backlog/test_rp_ctas_subquery.py::test_a_create_that_fills_nothing_runs_no_subquery,
tests/backlog/test_rp_ctas_subquery.py::test_refresh_runs_the_subquery_again).

Filed RP-484, found while working it: each column of a materialized view is
listed twice in `pg_attribute`
(tests/backlog/test_rp_ctas_subquery.py::test_matview_columns_listed_once).
