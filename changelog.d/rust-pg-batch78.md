### The Rust PostgreSQL server: every duplicate key waits, constraint index parameters, and plans that run nothing

A primary key's or unique constraint's index now keeps the storage parameters
it was given, as any other index does. That covers `PRIMARY KEY WITH
(fillfactor = 60)`, `UNIQUE (b) WITH (...)` and `ALTER TABLE ... ADD
CONSTRAINT ... WITH (...)`. `pg_class.reloptions` and `pg_get_indexdef` show
them, and `ALTER INDEX` changes them on such an index too.

A duplicate-key insert now waits for the other inserter in every form, as
PostgreSQL's does:
- on a UNIQUE column, not only on the primary key;
- from inside a function that a constant `SELECT` calls;
- with a `serial` column drawing from a sequence.

`EXPLAIN` without `ANALYZE`, and `PREPARE`, no longer run a function they
plan. `EXPLAIN`'s `TIMING` and `SUMMARY` options are honoured.

#### Fixed

- `secantus-pgplan` / `secantus-pg`: a constraint's index's `WITH (...)` is
  checked as its access method's options and kept with the constraint (the
  primary key's on the table, a UNIQUE or EXCLUDE constraint's on the
  constraint).
  - `ADD PRIMARY KEY USING INDEX` carries the index's options over.
  - A renamed constraint keeps them.
  - `ALTER INDEX ... SET` / `RESET` on the constraint's index rewrites
    them inside the transaction.
  - Fixes RP-265 (`tests/backlog/test_rp_batch77.py::test_scenario[a
    constraint's index keeps its storage parameters]` and `[a constraint's
    storage parameters through ALTER TABLE]`).
- `secantus-pgcatalog`: `UniqueConstraint` gains `reloptions`, recorded only
  when set.
- `secantus-pg`: a duplicate key waits in every form. Fixes RP-255
  (`tests/backlog/test_rp_isolation.py::test_unique_insert_waits`).
  - `secantus-storage` (shared with the MongoDB server): a UNIQUE index's
    claimed value is recorded as a held key row, as an `_id` claim is, so a
    writer colliding on it can find the transaction holding it.
  - A write that collides while its statement is planned (`select f()`
    folds the call) waits and is planned again.
  - A sequence's advance is not counted as a commit a REPEATABLE READ
    snapshot misses.
- `secantus-pgplan`: an EXPLAIN without ANALYZE, or a PREPARE, plans
  without calling a user function or drawing a sequence
  (`planning_without_running`). Fixes RP-266
  (`tests/backlog/test_rp_batch78.py::test_scenario[EXPLAIN and PREPARE run
  no function]`).
- `secantus-pg`: EXPLAIN's `TIMING` (refused without ANALYZE) and `SUMMARY`
  options. Its JSON gives the actual startup and total times and an empty
  `Triggers` group, as PostgreSQL's does. Fixes RP-267
  (`tests/backlog/test_rp_batch78.py::test_scenario[EXPLAIN's TIMING and
  SUMMARY]`).
