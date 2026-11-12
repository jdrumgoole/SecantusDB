### The Rust PostgreSQL server: a cursor computes its rows as they are fetched

A `DECLARE CURSOR` now computes its select list only for the rows a FETCH
reaches, as PostgreSQL runs a portal's plan as it is fetched. Before, the
DECLARE computed every row at once. A row no FETCH ever reached could fail
the DECLARE, and the cursor was then never made. Now `declare c cursor for
select 1/(k - 1) from (values (2), (1)) s(k)` followed by `fetch 1 from c`
answers `1`, as on PostgreSQL 15.19. The second row's division by zero is
raised by the FETCH that reaches it.

A `WITH HOLD` cursor declared in a block computes the rest of its rows at
COMMIT, as PostgreSQL's `PreCommit_Portals` does. An error there fails the
COMMIT and rolls the block back. The cursors that block declared go with
it.

#### Fixed

- `secantus-pg`: a cursor keeps its DECLARE's row stream unpolled
  (`portal_stream::PendingRows`). FETCH, MOVE, a scroll back and a
  PL/pgSQL FETCH compute only the rows they reach. The rows already
  computed are kept, so moving back reads them again rather than computing
  them twice. Their typed values are captured as they are computed, for a
  binary FETCH. The query itself still runs at the DECLARE, in its
  snapshot. Two kinds of cursor are still computed whole there: a simply
  updatable one, whose rows' ids are captured with them, and a `WITH HOLD`
  one declared outside a block.
- `secantus-pg`: a failed COMMIT dropped neither the `WITH HOLD` cursors
  its block declared nor their rows. They are now dropped, as
  `AtAbort_Portals` drops them.

Fixes RP-744 (`tests/backlog/test_rp_found_rp732.py::test_a_cursor_computes_its_rows_as_they_are_fetched`).

Filed while working it: RP-895 to RP-901. These are the cursor shapes still
computed at the DECLARE: function calls, `nextval` and scalar subqueries in
the select list, set operations, and a WHERE or Sort. Also filed: zero-count
FETCH, a duplicate DECLARE that leaves the block usable,
`pg_cursors.statement`, and a FETCH from a `refcursor` parameter.
