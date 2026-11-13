### A catalog read through a covering index reads the visibility map, as PostgreSQL's does

When a query reads a system catalog through an index that holds every column
it asks for -- `select relname from pg_class where relname = 'orders'` --
PostgreSQL runs an index-only scan. It reads the visibility map's page once,
and goes to the heap only for a row whose page the map does not mark
all-visible. The Rust PostgreSQL server counted these scans in
`pg_statio_sys_tables` as plain index scans: no visibility-map page, a heap
block for every row however all-visible its page, and nothing for a row
written after `initdb`, such as a new table's own `pg_class` row. A range
over `pg_class` names came out at 199 heap blocks where PostgreSQL 15.19
reads 17. These scans are now counted the way `IndexOnlyNext` makes
them, `idx_tup_fetch` included. Where DDL's own catalog rows go, and which
`initdb` pages they clear in the visibility map, is not modelled yet.

#### Fixed

- `secantus-pg`: an executor catalog scan through a covering index is an
  index-only scan (`catcache::Scan::IndexOnly`), and
  `catalog_btree::index_only_reads` counts the visibility map's page (none
  when the heap has no map fork), the heap blocks off all-visible pages, and
  each row written after `initdb`. EXPLAIN's `Heap Fetches` uses the same
  count.
- Part of RP-291 (tests/backlog/test_rp_catalog_reads.py::test_an_index_only_catalog_scan_reads_the_visibility_map);
  the item stays open for where DDL's rows go
  (`tests/backlog/test_rp_catalog_reads.py::test_a_page_ddl_wrote_to_is_not_all_visible`).
- Filed RP-935: a catalog read directly is scanned by its leading
  equalities alone, not by every clause's index path.
