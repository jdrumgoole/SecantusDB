### Rust PG server: a database-wide ANALYZE correlates a catalog column with PostgreSQL's heap order

`compute_scalar_stats` correlates a column's sort order with the order
`acquire_sample_rows` read the rows in, block by block through the heap.
A plain `ANALYZE` measured the built-in catalogs over this server's own row
order, so their `pg_stats.correlation` (and the index-scan costs that read
it) were not PostgreSQL's. The rows now go in the heap order the catalog
model keeps: `initdb`'s rows where it left them (the live tuple a HOT
chain's root redirects to, not the root its index entry names), and the
rows CREATE TABLE and CREATE DATABASE wrote since, where they were placed.
A row whose place is not modelled yet (RP-291) goes after them.

#### Fixed

- After `create table zzzz_rp313 (a int)` and `analyze` in a fresh
  database, `pg_class`'s correlations are PostgreSQL 15.19's (`oid`
  0.6547713, `relkind` 0.7882681, `relname` -0.12838678, `relnatts`
  0.67018104; this server answered 0.9854369, 0.8315357, 0.1999988 and
  0.50667804), as are `pg_type`'s `typinput`, `typlen` and `typname`.

Fixes RP-358 (tests/backlog/test_rp_catalog_reads.py::test_a_catalog_columns_correlation_follows_the_heap,
`test_a_catalog_columns_correlation_counts_the_new_rows_where_they_went`).
Files RP-1098: a new table's array type's oid sorts after its row type's,
which moves `pg_type.oid`'s correlation in its last digit.
