### The Rust PostgreSQL server: a constraint's index keeps its storage parameters

A primary key's or unique constraint's index now keeps the storage parameters
it was given, as any other index does. That covers `PRIMARY KEY WITH
(fillfactor = 60)`, `UNIQUE (b) WITH (...)` and `ALTER TABLE ... ADD
CONSTRAINT ... WITH (...)`. `pg_class.reloptions` and `pg_get_indexdef` show
them, and `ALTER INDEX` changes them on such an index too.

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
