### Rust PG server: a materialized view's columns are listed once in pg_attribute

A client that introspected a materialized view's columns through
`pg_attribute` saw each of them twice on the Rust PostgreSQL server, once for
the table that holds the view's rows and again as though it were a plain
view. PostgreSQL lists each once, and now so does the Rust PostgreSQL server,
with or without `WITH NO DATA`.

#### Fixed

- `secantus-pg`: `pg_attribute` lists a materialized view's columns from its
  table only, where PostgreSQL stores them, instead of also from its query
  output. Fixes RP-484 (tests/backlog/test_rp_ctas_subquery.py::test_matview_columns_listed_once)

#### Found

- RP-489: a CREATE TABLE AS or CREATE MATERIALIZED VIEW column loses its
  expression's type modifier (`atttypmod` -1 for `2.5::numeric(5,2)`, where
  PostgreSQL keeps 327686).
- RP-490: `pg_attribute` lists no system columns (`ctid`, `xmin`, ...
  `tableoid`) for a table or a materialized view.
