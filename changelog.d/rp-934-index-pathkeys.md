### An ORDER BY an index already gives is not counted as a sort

When the index scan a statement uses returns its rows in the order its ORDER
BY asks for, PostgreSQL 15.19 plans no Sort, so it never looks up the sort's
support function; and whenever a query has pathkeys it reads each B-tree
key's equality operator while building the index's own pathkeys. The Rust
PG server's catalog-read counters now follow both: `WHERE relname = 'rpc_t'
ORDER BY relname` counts no `pg_amproc` lookup, and `ORDER BY relnamespace`
counts `oid_ops`' equality.

#### Fixed

- Rust PG server: catalog reads for an ORDER BY over one relation follow
  `build_index_pathkeys` and skip the Sort's support lookup when the
  chosen index scan gives the order.
  Fixes RP-934 (tests/backlog/test_rp_catalog_reads.py::test_gap[an ORDER BY an index gives])
