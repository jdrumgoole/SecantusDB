### Rust PostgreSQL server: timestamptz, temp-table race, pgjdbc metadata

pgjdbc against the Rust PostgreSQL server went from 84 failures to 22 (7,354 tests); pgx stays at 377 passed / 0 failed. Every fix is checked against PostgreSQL 15 and pinned in the `b39_fixes`, `b39_datetime` and `b39_catalog` corpora.

#### Fixed

- A timestamptz copied from a column, a bound parameter or an array parameter was re-read as a wall clock. Under summer time that moved the value by an hour, silently.
- A newly created temp table was intermittently not found (42P01) under parallel sessions. The catalog version was bumped before the creating transaction committed.
- `ROLLBACK TO SAVEPOINT` undoes `SET` / `SET LOCAL`.
- `pg_typeof(nextval(...))` no longer advances the sequence twice.
- Unary `-` / `+` on money is refused.
- Binary money parameters are decoded.
- Notices are sent while a statement waits on a table lock. `LOCK TABLE` works inside a function.
- A comma-join with no join keys in `FROM` now hashes the WHERE equalities. pgjdbc's foreign-key metadata query no longer runs for minutes.
- Date/time:
  - fractional seconds round half-to-even;
  - `+hhmm` offsets are accepted;
  - `timetz ± interval` works;
  - BC binary dates decode.
- Binary `varchar[]` / `bpchar[]` / `name[]` results.
- `LIKE` with a parameter is described as boolean.

#### Added

- PostgreSQL's read-only internal settings, such as `max_index_keys`.
- `pg_get_keywords()`.
- Expression and partial indexes in `pg_index` / `pg_get_indexdef`.
- `ADD PRIMARY KEY USING INDEX`.
- `COMMENT ON DOMAIN`.
- Pseudo-types in `pg_type`.
- `lseg ?# box`.
- `COPY ... HEADER`.
- PL/pgSQL `$n` argument references.
- The pgjdbc runner creates a `test` role, as pgjdbc's own CI does.
