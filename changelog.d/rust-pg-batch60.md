### Rust PostgreSQL server: B-tree indexes have PostgreSQL's pages

An index's size, in `pg_relation_size` and in its `pg_class` row, is the
pages PostgreSQL 15's nbtree would have, kept across a restart.

#### Fixed

- Each B-tree index's pages are modelled (RP-196).
  - **What changed.** Before, an index's `pg_class` row read 0 pages and
    -1 tuples, and `pg_relation_size` of an index answered 0.
  - **CREATE INDEX** builds leaves to the fillfactor, with a high key per
    page, and internal pages to 70%. Equal keys go into posting lists.
  - **Inserts** fill a leaf, deduplicate it when full, and split it where
    `_bt_findsplitloc` would. That covers the rightmost leaf at the
    fillfactor, other leaves by balance, a composite key's ascending runs
    after the new item, and a page of one value near its end.
  - **VACUUM's index pass** removes the dead tuples' TIDs and keeps the
    pages. A page it empties is deleted, and reused only after a later
    VACUUM recycles it.
  - **`pg_class`** records what PostgreSQL records:
    - CREATE INDEX, an added UNIQUE or PRIMARY KEY, a constraint at
      CREATE TABLE, REINDEX, VACUUM FULL and CLUSTER record the build;
    - a VACUUM that ran its index pass records the TIDs left;
    - ANALYZE records the rows each index covers;
    - TRUNCATE records 0 pages and -1 tuples.
  - **Across a restart** the pages are written with the heap's (RP-193),
    at most once a second and at a clean shutdown. A row placed after the
    last write is added when the index is next touched.
  - **What is left.** PostgreSQL 14's bottom-up deletion is not modelled,
    nor deletion of tuples scans marked dead. So repeated UPDATEs that
    leave a key unchanged grow its index more here.

  Fixes part of RP-196 (tests/backlog/test_rp_batch49.py::test_scenario[pg_class shows an index's size],
  tests/backlog/test_rp_batch60.py::test_scenario[CREATE INDEX builds leaves to the fillfactor],
  tests/backlog/test_rp_batch60.py::test_scenario[CREATE INDEX deduplicates equal keys],
  tests/backlog/test_rp_batch60.py::test_scenario[inserts split leaves in key order and out of it],
  tests/backlog/test_rp_batch60.py::test_scenario[a composite key in ascending runs splits after the new item],
  tests/backlog/test_rp_batch60.py::test_scenario[inserted duplicates deduplicate],
  tests/backlog/test_rp_batch60.py::test_scenario[NULLs and INCLUDE columns and a partial index],
  tests/backlog/test_rp_batch60.py::test_scenario[an index's pg_class row through its life],
  tests/backlog/test_rp_batch60.py::test_scenario[VACUUM's index pass removes dead tuples and keeps the pages],
  tests/backlog/test_rp_batch60.py::test_an_index_keeps_its_pages_across_a_restart).
- `pg_relation_size` and `pg_total_relation_size` take an index's name as
  well as a table's.
