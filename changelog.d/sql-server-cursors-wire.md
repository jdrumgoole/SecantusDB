### SQL: server-side cursors over the wire, pg_cursors, pg_prepared_statements

psycopg's `ServerCursor` works end-to-end. A `DECLARE`d cursor is a portal in
the v3 protocol, and psycopg's first move after the DECLARE is a wire
`Describe('P', name)` — which our extended-protocol session answered with
`34000 portal does not exist`. The portal Describe (and Close) now fall back
to the session's DECLAREd cursors, parameterized declarations substitute
their `$N` placeholders inside the raw `DECLARE … FOR SELECT $1` command
text, and the session's cursors and prepared statements surface in new
`pg_cursors` / `pg_prepared_statements` catalog tables. psycopg's
test_cursor_server + test_prepared move 26 → 102 passing.

#### Added

- `pg_catalog.pg_cursors` (name / statement / is_holdable / is_binary /
  is_scrollable / creation_time, from the session's open cursors) and
  `pg_catalog.pg_prepared_statements` (SQL-level `PREPARE`d plus the
  connection's wire-Parse statements, exposed via `Session.wire_prepared`).

#### Fixed

- `pgextended.py`: `Describe('P', name)` on a DECLAREd cursor returns its
  RowDescription; `Close('P', name)` destroys the cursor.
- `planner.py`: `substitute_parameters` also substitutes `$N` textually
  inside a raw `exp.Command` tail (DECLARE bodies aren't parsed trees).
