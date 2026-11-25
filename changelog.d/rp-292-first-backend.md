### The first backend after a start builds the relation cache init files

On `secantusd-pg`, the catalog scans `pg_stat_sys_tables` and
`pg_stat_sys_indexes` count for the first session after a start, and for the
first session in each further database, now match PostgreSQL 15.19. Before
this change the model counted every backend as if the relation cache init
files were already there.

#### Fixed

- A start leaves no relation cache init file, as `StartupXLOG` removes
  them. The first backend after it therefore builds each one, transcribed
  from `relcache.c` / `catcache.c`:
  - heap scans of `pg_authid` and `pg_database`, until the critical shared
    indexes are built;
  - the seven critical local indexes, built by heap scans of `pg_class`,
    `pg_attribute`, `pg_index` and the `oid_ops` / `int2_ops` operator
    classes;
  - the six critical shared indexes;
  - every `formrdesc`'d catalog's own `pg_class` row;
  - every system cache's catalog and index (`InitCatalogCachePhase2`).

  Its `select 1` and an observer's start read 249 `pg_attribute` index
  scans, 129 `pg_class` ones and 80 `pg_index` ones, as PostgreSQL's do.
  Before, the model counted 8, 4 and 2. The first backend in a second
  database builds only that database's file.
- Fixes RP-292 (`tests/backlog/test_rp_catalog_reads.py::test_the_first_backend_after_a_start`,
  regression test `test_the_first_backend_in_a_second_database`, crate test
  `catcache::tests::a_start_leaves_no_init_file_until_a_backend_writes_them`).
  Files RP-1096: the logical replication launcher's `pg_subscription` scans
  are not counted. Files RP-1097: an invalidation, such as a database-wide
  `ANALYZE`, never removes the init files.
