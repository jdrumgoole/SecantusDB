### Rust PG server: a procedure can COMMIT inside a cursor FOR loop

A PL/pgSQL `for x in c loop ... commit; end loop` in a procedure now reads
on through its cursor after each COMMIT or ROLLBACK, as PostgreSQL 15.19
does: the loop pins the cursor it walks (`exec_for_query`), and a
procedure's transaction end holds a pinned cursor rather than closing it
(`HoldPinnedPortals`). The Rust PostgreSQL server closed the cursor at the
COMMIT, so the loop's next row failed with 34000 `cursor "c" does not
exist`, the usual shape of a batch job that commits as it goes.

#### Fixed

- A COMMIT or ROLLBACK (with or without AND CHAIN) inside a cursor FOR loop
  in a procedure or DO block holds the loop's cursor: the loop goes on, a
  FETCH in its body still moves it, `pg_cursors` lists it (not holdable)
  until the loop ends, and other open cursors are closed as before.
- A `CLOSE c` of the cursor a FOR loop is walking, by PL/pgSQL or by
  `execute 'close c'`, and an `execute 'close all'` in its body, are 24000
  `cannot drop pinned portal "c"` (`PortalDrop`), where the cursor was
  dropped and the loop then failed with 34000.

Fixes RP-823 (tests/backlog/test_rp_found_rp809.py::test_a_cursor_loops_portal_is_pinned)
