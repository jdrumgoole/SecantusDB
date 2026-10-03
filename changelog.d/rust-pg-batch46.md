### Rust PostgreSQL server: PostgreSQL-equivalent commit sync on macOS, PL/pgSQL error context

#### Changed

- On macOS, durable commits sync the log with `O_DSYNC`, which is what PostgreSQL's macOS default `open_datasync` does. They no longer use a full drive-cache flush (`F_FULLFSYNC`). An acknowledged commit still survives a process kill (0 losses in 20 kill runs), but not a power loss, the same as PostgreSQL. Durable autocommit UPDATE dropped from 7.9 ms to 149 µs, against PostgreSQL 15's 82 µs. Linux and the Rust MongoDB server are unchanged.

#### Fixed

- `a LIKE 'x'` over an integer in the select list returned NULL. It is now 42883 with a position.
- A FROM-list subquery that references a sibling (`FROM t x, (SELECT x.a) s`) is 42P01 with PostgreSQL's hint.
- PL/pgSQL errors carry PostgreSQL's CONTEXT stack, through nested calls, EXECUTE, triggers and DO blocks.
- `pg_get_viewdef`:
  - prints implicit casts for the resolved overload;
  - prints `EXTRACT` in its own syntax;
  - wraps by column width.
