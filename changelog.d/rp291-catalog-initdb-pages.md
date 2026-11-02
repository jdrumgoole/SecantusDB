### Rust PG server: the built-in catalogs' initdb pages and free space maps are recorded

A step towards placing catalog rows added after `initdb` (RP-291): each
built-in catalog's heap pages are now recorded as PostgreSQL 15.19's
`initdb` leaves them, with free space, line pointers, redirects and
heap-only tuples. Each catalog's free space map fork is recorded exactly,
including `fp_next_slot` and stale upper nodes. The heap page model builds
each catalog's heap from them. A new table's `pg_class`, `pg_type` and
`pg_attribute` rows are placed on the blocks and line pointers PostgreSQL
picks for them. No count changes yet: placing DDL's rows into these
models is the next slice.

#### Added

- `catalog_heap::initdb_heap_model`, over the pages and FSM records that
  `tools/probes/pg15_system_catalog_dump.py` now writes into
  `pg15_catalog_heaps.txt`. Part of RP-291
  (`crates/secantus-pg/src/catalog_heap.rs::tests::a_new_tables_catalog_rows_go_where_postgresql_puts_them`).
