### Rust PG server: a catalog scan counts the heap blocks behind `initdb`'s rows

`pg_statio_sys_tables` now counts the heap blocks a catalog scan reads, as
PostgreSQL 15.19 does, for the rows `initdb` left: an index scan fetches the
block of each tuple it returns (again only after another block), and a
sequential scan reads every block of the heap, at the places
`pg15_catalog_heaps.txt` records. A new session's start, a `pg_type` lookup,
a `regrole` input and repeated scans of `pg_namespace` and `pg_am` match
PostgreSQL's counts block for block.

#### Fixed

- Catalog heap blocks for `initdb`'s rows (RP-291, part): regression test
  `tests/backlog/test_rp_catalog_reads.py::test_catalog_heap_blocks`. RP-291
  stays open for the catalog rows added after `initdb`.

#### Filed

- RP-316: a built-in catalog's first reads in a session differ from
  PostgreSQL's (`tests/backlog/test_rp_catalog_reads.py::test_gap[a built-in catalog's first reads]`).
