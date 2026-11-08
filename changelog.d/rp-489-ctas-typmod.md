### Rust PG server: CREATE TABLE AS keeps each column's type modifier

A table made by `CREATE TABLE AS` or `CREATE MATERIALIZED VIEW` lost its
columns' type modifiers on the Rust PostgreSQL server: `create table ct as
select * from t` over a `numeric(5,2)`, `varchar(10)` or `char(3)` column
made a bare `numeric`, `varchar` or `bpchar` column (`atttypmod` -1), so
`\d` and `format_type` printed the wrong type. PostgreSQL copies the query's modifiers into the new table, and the
Rust PostgreSQL server now does the same. That covers a base-table column, a
cast such as `2.5::numeric(5,2)`, and a VALUES list's common modifier, with
or without `WITH NO DATA`.

#### Fixed

- `secantus-pg`: `copy_query_fields` carries each output column's type
  modifier, and CREATE TABLE AS stores it as the new column's `atttypmod`.
  Fixes RP-489 (tests/backlog/test_rp_ctas_subquery.py::test_ctas_column_keeps_its_typmod,
  tests/backlog/test_rp_ctas_subquery.py::test_ctas_typmod_by_source)

#### Found

- RP-493: a column's type modifier is lost through a subquery, a CTE, a
  join, a UNION, GROUP BY, or a CASE, COALESCE or ARRAY over operands that
  share one. Both the RowDescription and CREATE TABLE AS show -1 there.
- RP-494: CREATE TABLE AS over a `bit` or `varbit` column is refused with
  0A000.
