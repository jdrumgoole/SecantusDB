### The Rust PostgreSQL server enforces UNIQUE

`CREATE TABLE t (tag text UNIQUE)` was accepted and then ignored: the
constraint was recorded nowhere and a second equal value inserted where
PostgreSQL answers `23505`. That is worse than a missing feature. A refusal
is honest and you meet it on the first run; this let a user believe a
uniqueness guarantee the server was not providing, and the duplicates were
already stored by the time anyone noticed.

Enforcement is a storage unique index rather than a check before each write,
because a probe read cannot see a value another transaction committed after
the writer's snapshot, nor one a second writer is inserting right now — so
WiredTiger arbitrates instead. SQL NULLs stay distinct, as SQL requires: the
index carries a partial filter excluding NULL from every column, which a
`sparse` index would not achieve, because a SQL NULL is stored as an explicit
null rather than a missing field.

The error surface was measured against PostgreSQL 14.13 — the constraint
PostgreSQL would have generated (`<table>_<column>_key`, or the declared name),
and `DETAIL: Key (a, b)=(1, 2) already exists.` for a multi-column one.

#### Added
- `secantus-pgplan`: column-level and table-level `UNIQUE`, with PostgreSQL's
  constraint-name generation and `DEFERRABLE` / `INITIALLY DEFERRED`.
- `secantus-pgcatalog`: `UniqueConstraint`, in the Python server's on-disk
  shape key for key, so a table created by one server reads back in the other.

#### Fixed
- `secantus-pgserver`: an `UPDATE` that violated any unique constraint —
  including the pre-existing `PRIMARY KEY` one — surfaced as `could not
  update: E11000 duplicate key error on index ...`, leaking the MongoDB
  persona through the PostgreSQL one with no SQLSTATE a client could branch
  on. Both write paths now render PostgreSQL's `23505`.
- `secantus-pgplan`: `DEFERRABLE` attached to the most recent FOREIGN KEY
  regardless of what it actually qualified, so `UNIQUE ... DEFERRABLE` would
  have marked an unrelated foreign key deferrable.
