### The Rust PostgreSQL server enforces NOT NULL, CHECK and FOREIGN KEY

`CREATE TABLE` on the Rust PostgreSQL server now records its NOT NULL, CHECK
and FOREIGN KEY constraints in the shared catalog and enforces them on every
INSERT, UPDATE and DELETE, answering what PostgreSQL 16 answers: `23502` with
the failing column, `23514` with the constraint's name, `23503` on the child
side and — for NO ACTION / CASCADE / SET NULL — the parent side, each with the
`Failing row contains (...)` / `Key (...)=(...)` detail and the schema, table,
column and constraint diagnostic fields a driver reads. A `DEFERRABLE
INITIALLY DEFERRED` key is checked at COMMIT: the COMMIT reports the
violation, the transaction rolls back and the connection is left idle, as it
is on PostgreSQL. Unnamed constraints take PostgreSQL's generated names
(`<table>_<column>_check`, `<table>_check1`, `<table>_<column>_fkey`), and a
CHECK naming a missing column or a foreign key without a unique target is
refused at CREATE (`42703`, `42830`).

#### Added

- Rust PostgreSQL server: NOT NULL (`23502`), CHECK (`23514`) and FOREIGN KEY
  (`23503`, immediate and `INITIALLY DEFERRED` to COMMIT) enforcement with
  PostgreSQL's messages, details and diagnostic fields; `ON DELETE CASCADE` /
  `SET NULL`; temp tables report a `pg_temp` schema in diagnostics.
- `secantus-pgcatalog`: `TableDef` carries `temp`, `check_constraints` and
  `foreign_keys` in the Python server's document shape.
