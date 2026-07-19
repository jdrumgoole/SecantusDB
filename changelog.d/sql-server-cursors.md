### SQL server: binary and scrollable server-side cursors

psycopg's `ServerCursor` / `RawServerCursor` (DECLARE … CURSOR / FETCH /
MOVE over the wire) work in binary as well as text, and honour scroll
semantics. A binary `FETCH … FROM <name>` rides the extended protocol, so
Describe on the FETCH portal now reports the cursor's columns instead of
NoData — previously the server sent DataRows with no prior RowDescription, a
protocol violation the client rejected. `DECLARE … NO SCROLL` is enforced
(backward movement raises 55000, a psycopg `OperationalError`) and `SCROLL`
allows it; a negative bare count (`FETCH -2`, `MOVE -1`) scans backward in
the default direction, `FORWARD -n` / `BACKWARD -n` flip direction, and
`MOVE ABSOLUTE 0` repositions before the first row (not at the end) like
Postgres. A DECLARE body that isn't a row-returning query (`wat`, a DDL
statement) raises 42601 (ProgrammingError) rather than 0A000. psycopg's
`test_cursor_server.py` goes from 15 failed / 7 errored to 0.

#### Added

- `_Cursor.scrollable` (SCROLL / NO SCROLL); `_moves_backward` gates NO
  SCROLL cursors.
- `describe_statement` reports a FETCH portal's columns (binary server
  cursors) and NoData for MOVE.

#### Fixed

- Negative FETCH/MOVE counts scan backward; `MOVE ABSOLUTE 0` positions
  before-first; a non-query DECLARE body is a syntax error.
