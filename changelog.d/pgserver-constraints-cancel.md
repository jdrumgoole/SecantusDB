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

- Rust PostgreSQL server: `CancelRequest` interrupts the running statement
  (`57014 canceling statement due to user request`, connection left idle);
  `pg_stat_activity` shows each backend's state and running query; and
  `idle_in_transaction_session_timeout` / `idle_session_timeout` are
  validated, rendered (`60000` shows as `1min`) and enforced — the session
  ends with FATAL `25P03` / `57P05` and the connection closes, as on
  PostgreSQL 16.
- Rust PostgreSQL server: databases. The startup packet's `dbname` is checked
  before `AuthenticationOk` — an unknown name is FATAL `3D000 database "x"
  does not exist` (a failed connect in libpq, not a failed first query) and
  `template0` is `55000` — against a registry of `postgres` / `template1` /
  `template0`, the daemon's `--database NAME` flags and `CREATE DATABASE`;
  `DROP DATABASE` (with PostgreSQL's `25001`, `42P04`, `3D000` / `IF EXISTS`
  notice, `55006` and `42809`) drops the data too; `pg_database` lists the
  set and `current_database()` / `current_catalog` name the connected one.

- Rust PostgreSQL server: `GROUP BY` over an expression (`length(data)`,
  `col is null`, `n + 1`) or a select-list position (`GROUP BY 1, 2`), with
  the key matched to the projected expression by structure and to `ORDER BY`
  by position, alias or expression; `IS [NOT] NULL` as a value, including
  PostgreSQL's row rule (`row(1, null)` is neither null nor not null); and a
  FROM-less `select unnest(array)` as one row per element in a column named
  `unnest` of the element type.

#### Fixed

- Rust PostgreSQL server: a long statement no longer stalls every other
  connection — execution runs off the async runtime's I/O thread, so a
  cancel request (or any other client) is served while `pg_sleep` runs.

- Rust PostgreSQL server: every extended-protocol statement between two
  `Sync`s runs in one transaction that the `Sync` commits, as on PostgreSQL —
  an error in a pipeline now rolls back the earlier statements of its group
  (libpq's `PIPELINE_ABORTED` batch is all-or-nothing), `BEGIN` inside a
  group turns it into a block, and `DECLARE` in a group is still `25P01`.
- Rust PostgreSQL server: a statement prepared without parameter types
  (libpq `PQprepare` with `nParams = 0`) sizes its parameters from the lexer,
  so `insert into t values ($1, $2)` no longer fails with `there is no
  parameter $1` — pg_query's node walk skips a VALUES list.
- Rust PostgreSQL server: the first DDL on a fresh store no longer fails a
  second connection with a WiredTiger `WriteConflict`. The `__sql_*` catalog
  collections were registered lazily inside whichever block first needed one,
  and a block that began before the row landed could not see it; they are
  now created before a transaction handle opens. The enum / composite type
  oid counter is likewise advanced outside the block, like PostgreSQL's OID
  counter, so two open `CREATE TYPE` blocks no longer conflict (a rolled-back
  block just skips an oid).
