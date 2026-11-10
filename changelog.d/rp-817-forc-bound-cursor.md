### Rust PG server: a PL/pgSQL FOR loop runs over a bound cursor

`for x in c(1) loop ... end loop` over a cursor declared `c cursor (k int)
for select ...` opens the cursor with its arguments, walks its rows into the
loop variable and closes it when the loop ends, as PostgreSQL 15.19 does
(`exec_stmt_forc`). The Rust PostgreSQL server refused every such loop with
0A000 `the statement forc in a PL/pgSQL function is not supported yet`.

#### Fixed

- A FOR loop over a bound cursor, with or without arguments (positional or
  named), runs. The portal is named by the cursor variable and shows in
  `pg_cursors` while the loop runs; a FETCH in the body moves the loop with
  it; EXIT, CONTINUE, RETURN and a caught error leave no portal behind;
  FOUND is set when the loop ends, and ROW_COUNT is left alone.
- A bound cursor variable holds its own name before any OPEN, as PostgreSQL
  15's grammar makes its default (`curname_def`): it reads `c`, and one set
  to NULL opens an unnamed portal and, after a FOR loop, is NULL again.
- An OPEN or FOR of a cursor whose portal is already open is 42P03 before
  the cursor's arguments are evaluated, as PostgreSQL checks it first.

Fixes RP-817 (tests/backlog/test_rp_found_rp809.py::test_for_over_a_bound_cursor)
