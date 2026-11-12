### The first catalog query after DDL no longer rebuilds PostgreSQL's own columns

After any DDL statement, the Rust PostgreSQL server's next catalog query rebuilt
`pg_attribute` whole. That included the 3,054 rows describing PostgreSQL's own
relations, which no statement can change, and the server built it twice: once
for the query and once for the walk that counts its catalog reads. In a debug
build, psql's `\d` collation query took 0.12 s straight after a `CREATE TABLE`
and 0.007 s after that. PostgreSQL itself takes 0.9 ms and then 0.2 ms.

Those rows are now built once per process and shared, not copied into each
read, so the same query takes about 0.02 s after DDL. Filling the rows of the
relations users create also no longer searches the whole table list for every
row, which had made it quadratic in the number of tables. With 400 tables the
first query after DDL fell from 1.79 s to about 0.21 s. It still grows with the
schema, because every user relation's rows are rebuilt; that remainder stays
open as RP-715.

#### Fixed

- `secantus-pg`: `pg_attribute`'s rows for PostgreSQL's own relations are
  built and filled once per process (`system_relation_attributes`) and kept
  beside each read's own rows (`CatalogRows`) rather than copied into them.
  The catalog-read walk counts them in place.
- `secantus-pg`: `fill_catalog_columns` looks a row's relation up by oid in a
  map rather than scanning every table, and lists the collations once per fill
  rather than once per collated column.
- Fixes RP-715 in part (tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_what_the_next_does). The new gap test
  `test_the_first_catalog_query_after_ddl_does_not_grow_with_the_schema` keeps
  RP-715 open for the per-relation remainder. `tests/test_rust_pgserver_slice.py::test_system_relations_attributes_ignore_the_user_schema`
  pins that nothing a user creates changes the shared rows.
