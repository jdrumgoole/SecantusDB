### A `CREATE TABLE` costs the same however many tables there are

A `CREATE TABLE` on the Rust PostgreSQL server no longer grows with the
schema. Its hash-index check asked for the new table's oid through a map of
every relation's oid, which the statement's own catalog write invalidated, so
each `CREATE TABLE` rebuilt it over every composite. It now finds the one
relation by its key in the sorted catalog, as PostgreSQL searches `pg_class`
by name through its index: with 800 other tables a debug build's median
`CREATE TABLE` went from 2.9 ms to 1.6 ms.

#### Fixed

- `CREATE TABLE` no longer lists every relation to find its own oid.
  Fixes RP-928 (tests/backlog/test_rp_catalog_latency.py::test_create_table_costs_the_same_at_800_tables_within_a_tenth)
