### `test_catalog_blocks` measures in a fresh database, and the dead-space gap it hit has a test of its own

RP-291's regression test for the catalog heap blocks a primary-key lookup
reads failed twice on CI. It read 4 `pg_attribute` blocks where
PostgreSQL reads 3. It measured in the module's shared database, where
the catalog rows land after whatever DDL the module's earlier tests ran on
that server, and under xdist that history changes from run to run. On one
server the result is deterministic: the tenth run reads 4 every time.

#### Fixed

- `tests/backlog/test_rp_catalog_reads.py::test_catalog_blocks` measures in
  a fresh database (`after_ddl_cycles`), as the module's other heap-place
  tests do. PostgreSQL reads the same blocks there as in a fresh
  cluster's `postgres` database (private 15.20 cluster, 2026-11-26).
- Fixes RP-1111 (`tests/backlog/test_rp_catalog_reads.py::test_catalog_blocks`).

#### Found

- RP-291 gains a gap test for the divergence behind the flake. The model
  never frees the space a dropped table's catalog rows leave, so the tenth
  cycle of the module's DDL in one database reads 4 `pg_attribute` blocks.
  PostgreSQL prunes that space and reads 3 at every cycle from 1 to 14
  (`tests/backlog/test_rp_catalog_reads.py::test_a_dead_rows_space_takes_new_rows`).
