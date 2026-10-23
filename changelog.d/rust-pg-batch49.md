### Rust PostgreSQL server: `pg_class` shows a table's page and tuple counts

`pg_class.relpages`, `reltuples` and `relallvisible` are now what VACUUM,
ANALYZE and CREATE INDEX recorded, as on PostgreSQL 15.19. Before, they read
0, -1 and 0 for every table. The pages are the heap's own (`heap.rs`).

#### Fixed

- Fixes most of RP-196
  (`tests/backlog/test_rp_batch48.py::test_scenario[pg_class shows what VACUUM counted]`
  and `tests/backlog/test_rp_batch49.py::test_scenario[...]`: `pg_class's
  counts through CREATE INDEX, ANALYZE, VACUUM and TRUNCATE`, `pg_class's
  counts of a table the planner does not size` and `pg_class's counts of a
  partitioned table are its leaves'`):
  - **The recorded pages are the heap's.** VACUUM, ANALYZE, CREATE INDEX
    and the planner's own size estimate (`estimate_rel_size`) take
    `RelationGetNumberOfBlocks` from the heap model. For a table nothing has
    updated, this is the page count the rows' widths gave before.
  - **Every table is counted.** A table whose rows the planner does not size
    (numeric, arrays) is still counted, though the planner keeps declining
    to price it.
  - **Partitions.** VACUUM of a partitioned table counts each leaf, at any
    depth, and leaves the parent at `0 | -1`. A leaf's heap is derived from
    the root's rows; before, a restart lost it.
  - **The listing follows the counts.** A cached `pg_class` listing is
    dropped when a count is recorded, so a TRUNCATE's reset shows at once.

#### Known

- RP-196, what is left: an index's row still reads 0 and -1. Its pages need
  a model of the B-tree as inserts split it
  (`tests/backlog/test_rp_batch49.py::test_scenario[pg_class shows an index's size]`).
