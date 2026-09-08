### The Rust PostgreSQL server exposes `pg_cursors` and constant select-list columns

psycopg's server cursor (`ServerCursor` / `RawServerCursor`) does two things the
Rust `secantusd-pg` server could not answer. It reads the `pg_cursors` catalog —
directly, to confirm a cursor is gone after `CLOSE`, and internally as
`SELECT 1 FROM pg_catalog.pg_cursors WHERE name = ...` before closing a cursor it
did not declare — and every server-cursor test seeds its rows with a bare
literal, `SELECT 1 FROM generate_series(...)`. The first raised
`relation "pg_cursors" does not exist`; the second raised `AConst is not
supported yet`, because the select-list planner accepted a column, a cast, or a
scalar call but not a plain constant.

`pg_cursors` is now a virtual catalog table over the connection's open cursors,
with PostgreSQL's columns (`name`, `statement`, `is_holdable`, `is_binary`,
`is_scrollable`, `creation_time`); a `CLOSE` removes the row, and a cursor
declared `NO SCROLL` / `WITH HOLD` / `BINARY` reports it faithfully. A literal in
the select list — `SELECT 1 FROM t`, over a table or a `generate_series` — is now
a constant column named `?column?` (or its alias), one value per row, which is
how a client counts a source's rows through `cursor.rowcount` without reading
their values.

Measured against psycopg 3's own cursor suite (`test_cursor*.py`), the change
turns 38 previously-failing cases green, among them `test_context`, `test_close`,
`test_stolen_cursor_close`, `test_init_params`, `test_rownumber`, and
`test_scrollable`.

#### Added

- `secantus-pgserver`: a `pg_cursors` virtual catalog table backed by the
  connection's open-cursor map; `CursorState` now carries the catalog columns,
  captured at `DECLARE` from the cursor's options.
- `secantus-pgplan`: a `ColumnExpr::Const` select-list column, planned from an
  `AConst` target over a table or a `generate_series` source.
