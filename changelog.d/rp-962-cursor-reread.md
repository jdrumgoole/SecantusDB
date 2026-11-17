### A cursor over a table computes a volatile column again when it reads a row again

A SCROLL cursor whose select list calls a volatile function, such as
`nextval`, now projects a row again each time it reads it, as PostgreSQL's
scan node does: `fetch prior` and `fetch first` over a table or a VALUES
list draw a new value rather than answer the one first computed. A simply
updatable cursor over a table no longer computes its whole select list at
the DECLARE to capture each row's id for `WHERE CURRENT OF`: it computes
each row, and captures its id, as a FETCH reaches it.

#### Fixed

- A DECLARE of a simply updatable cursor drew every `nextval` in its select
  list; it now draws none, and `WHERE CURRENT OF` still names the row the
  cursor sits on.
- A SCROLL cursor over a table or VALUES read a row again as first
  computed; a volatile column is computed again.

Fixes RP-962 (tests/backlog/test_rp_found_rp744.py::test_a_cursor_over_a_table_recomputes_a_volatile_column_when_it_reads_a_row_again, tests/backlog/test_rp_found_rp744.py::test_a_simply_updatable_cursor_computes_its_rows_as_they_are_fetched)
