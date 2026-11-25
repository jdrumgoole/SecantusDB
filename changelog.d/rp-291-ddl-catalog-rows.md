### The Rust PG server reads a new table's catalog rows from where PostgreSQL puts them

`pg_statio_sys_tables` counts the heap blocks behind the catalog rows a
CREATE TABLE or CREATE DATABASE wrote, and an index-only catalog scan
fetches the `initdb` rows on a page such a row was written to, as
PostgreSQL 15 does.

#### Fixed

- A catalog lookup that returned a row CREATE TABLE wrote (a new table's
  `pg_class`, `pg_attribute`, `pg_type` or `pg_index` row) counted no heap
  block, so a session's first query of a new table read 4 `pg_class`, 3
  `pg_attribute` and 2 `pg_index` blocks fewer than PostgreSQL. Each
  database now keeps its catalogs' heap pages and free space maps as
  `initdb` left them and places every row CREATE TABLE writes, in
  PostgreSQL's order and at its `heap_form_tuple` length, with the HOT
  update a TOAST table makes; the cluster's `pg_database` heap takes
  CREATE DATABASE's row. DROP TABLE and DROP DATABASE leave them dead when
  they commit, as a rolled-back CREATE does.
- A write cleared no page's visibility map bit, so an index-only scan of an
  `initdb` row on a page DDL had written to did not fetch it from the heap.

Fixes RP-291, in part (tests/backlog/test_rp_catalog_reads.py::test_catalog_blocks,
tests/backlog/test_rp_catalog_reads.py::test_a_page_ddl_wrote_to_is_not_all_visible,
tests/backlog/test_rp_catalog_reads.py::test_ddl_cycles_write_the_pages_postgresqls_do).
