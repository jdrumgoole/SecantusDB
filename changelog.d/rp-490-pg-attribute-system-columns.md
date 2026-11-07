### Rust PG server: `pg_attribute` lists a relation's system columns

A table's `ctid`, `xmin`, `cmin`, `xmax`, `cmax` and `tableoid` answered a
`SELECT`, but `pg_attribute` had no row for any of them, so a client looking
for `attnum < 0` found nothing. PostgreSQL lists all six for every relation
with storage.

#### Fixed

- `pg_attribute` lists the six system columns (attnum -6 to -1) of every
  table, partitioned table and materialized view, as `heap.c`'s `SysAtt`
  defines them. Each is NOT NULL, with `attlen`, `attbyval`, `attalign` and
  `attstorage` taken from its type. A view, a composite type and an index
  still have none, as in PostgreSQL.
- A system column's `attstattarget` is 0, not -1. This applies to
  PostgreSQL's own catalogs' system columns as well.

Fixes RP-490 (tests/backlog/test_rp_ctas_subquery.py::test_pg_attribute_lists_system_columns,
tests/backlog/test_rp_ctas_subquery.py::test_system_columns_by_relation_kind).
Found while working it, and filed: RP-498, a sequence lists no columns in
`pg_attribute`.
