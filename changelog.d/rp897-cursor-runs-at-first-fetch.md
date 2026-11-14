### The Rust PostgreSQL server: a cursor runs its query at the first FETCH

A `DECLARE CURSOR` inside a block now only plans its query, as
PostgreSQL's `PortalStart` does. The query runs at the first FETCH that
needs a row. Before, the DECLARE ran it, so a WHERE or a Sort that raised
failed the DECLARE, and the cursor was never made. Now `declare c cursor
for select k from (values (2), (0)) s(k) where 1/k > 0` succeeds, and its
`fetch 1` raises the division by zero, as on PostgreSQL 15.19. A cursor
that is closed without a FETCH runs nothing at all.

The cursor still reads what the DECLARE's snapshot holds. A row the block
inserts or updates after the DECLARE is not the cursor's. Any other
statement of the block runs a cursor not run yet before it moves the
snapshot or writes, and an error from that run is kept for the FETCH that
reaches it.

#### Fixed

- `secantus-pg`: `portal_stream::PendingRows::Unstarted` holds a
  DECLARE's planned query, with the planner's row description
  (`describe_planned`) until the run gives its own. FETCH, MOVE, a
  PL/pgSQL FETCH and `cursor_to_xml` start it. Rows are now computed
  outside the cursor lock. Other statements start it first
  (`start_deferred_cursors`). A COMMIT runs a `WITH HOLD` one whole and
  closes the rest unrun. A simply updatable cursor and a `WITH HOLD`
  cursor declared outside a block still run at the DECLARE.
  Fixes RP-897 (tests/backlog/test_rp_found_rp744.py::test_a_cursor_runs_its_query_at_the_first_fetch;
  regression tests `test_a_deferred_cursor_reads_the_declares_snapshot`
  and `test_a_deferred_cursor_raises_at_its_fetch` in the same file).

#### Known gaps

- RP-961 filed: the executor still evaluates a cursor's whole WHERE when
  the first FETCH runs it, where PostgreSQL filters as it fetches.
