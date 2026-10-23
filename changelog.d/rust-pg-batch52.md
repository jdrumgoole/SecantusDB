### Rust PostgreSQL server: a block sees its own index definitions

Inside a transaction block, `pg_get_indexdef` and `shobj_description` now
answer for what the block itself created or commented, as PostgreSQL 15.19
answers. Before, they read the committed catalog alone until COMMIT.

#### Fixed

- Fixes RP-200
  (`tests/backlog/test_rp_batch51.py::test_scenario[pg_get_indexdef of an index the block created]`,
  `tests/backlog/test_rp_batch52.py::test_scenario[a block's own definitions, and no other session's]`):
  the planner's published tables of index definitions and shared comments
  were built before each statement took the block's own transaction, so an
  index the block created had no `pg_get_indexdef` (NULL) and a comment it
  wrote on a database had no `shobj_description`. A block that has written
  the catalog now builds them through its own transaction. Another session
  still sees neither, and after ROLLBACK neither is seen.
