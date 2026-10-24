### Rust PostgreSQL server: a sequential scan a LIMIT stops counts what it read

`pg_stat_user_tables.seq_tup_read` and `pg_statio_user_tables` count a
scan under LIMIT as PostgreSQL 15.19 does. A scan streamed inside a
transaction block is now counted at all.

#### Fixed

- A sequential scan that a LIMIT stops counts the tuples and pages it
  read before stopping, where it counted every visible tuple and page
  (RP-224).
  - **How it is counted.** PostgreSQL's scan stops at the row that
    completes OFFSET + LIMIT, in heap order. The count is the visible
    tuples up to that row's ctid, and the pages up to its block.
    `select * from t limit 1` reads 1 tuple and `... where g = 10 limit 1`
    reads 10.
  - **What it covers.** A LIMIT 0 never starts the scan. Rows that moved
    in the heap are counted in ctid order. A filter evaluated per row is
    applied before the count. Streamed and materialised results are both
    covered.
  - **Unchanged.** An ORDER BY, DISTINCT or window reads every row first,
    and still counts every row.

  Fixes RP-224 (tests/backlog/test_rp_batch55.py::test_scenario[a sequential scan under LIMIT reads only what it returns],
  tests/backlog/test_rp_batch63.py::test_scenario[a LIMIT stops a scan of moved rows at its last row]).
- A query streamed through a transaction block's own transaction
  (`begin; select g from t; ...`) counts its scan. Before, it was missing
  from `pg_stat_user_tables`.

  Fixes RP-238 (tests/backlog/test_rp_batch63.py::test_scenario[a scan streamed in a transaction block is counted]).

#### Known gaps

- A write counts one heap buffer access per row, where PostgreSQL's
  `heap_insert` counts its bulk-write ring and the accesses made when the
  heap extends (RP-239, open).
