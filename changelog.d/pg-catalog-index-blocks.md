### The PostgreSQL server counts the catalog index pages a statement reads

`pg_statio_sys_indexes` now rises as PostgreSQL 15.19's does when a session
reads its catalogs. Each built-in catalog index is the pages `initdb`
actually left on disk -- every tuple's key and heap TIDs, decoded from a
fresh 15.19 cluster's files. A catalog lookup reads the index's metapage on
the backend's first use of it, then the path down from the root and each
leaf it walks across. A new session's start, a catalog query and a query on
a table match PostgreSQL's block counts for every catalog index.

This is the first slice of RP-291. Catalog rows added after `initdb` are not
yet placed in the indexes, and the catalogs' heap pages are not yet
counted. Found on the way: `pg_statistic` holds none of `initdb`'s rows
(RP-311).

#### Fixed

- `secantus-pg` (`catalog_btree.rs`, `btree_model.rs`, `catcache.rs`,
  `catalog_reads.rs`): every catalog index scan carries its key and counts
  the pages it reads in `pg_stat_get_blocks_fetched` / `_hit`. An empty
  index's metapage is read twice per scan and never cached, as
  `_bt_metaversion` and `_bt_getroot` read it. The planner's
  `_bt_getrootheight` reads each catalog index's metapage.
- `tools/probes/pg15_system_catalog_dump.py` also writes
  `pg15_catalog_indexes.txt`: every built-in catalog index's B-tree pages,
  each leaf tuple's key (decoded from its bytes, a `name` key as the
  `cstring` `name_ops` stores) and heap TIDs, read from the cluster's
  files.

Fixes part of RP-291 (tests/backlog/test_rp_catalog_reads.py::test_catalog_index_blocks).
Files RP-311 (tests/backlog/test_rp_catalog_reads.py::test_builtin_statistics).
