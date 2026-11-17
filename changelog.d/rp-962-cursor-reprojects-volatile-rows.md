### A scroll cursor runs a volatile select list again when it reads a row again

PostgreSQL's scan node projects every row it returns, so a cursor that
reads a row again -- a backward FETCH, FETCH FIRST, FETCH 0, or the rewind
COMMIT performs to persist a WITH HOLD cursor -- runs a `nextval` or
`random()` in its select list again. The Rust PostgreSQL server kept the
rows it had computed and read them back. A cursor over a set-returning
function such as `generate_series` now keeps each row's source row and
moves as PostgreSQL's `DoPortalRunFetch` moves the executor, projecting
every row it steps onto; a WITH HOLD cursor projects its rows again when
COMMIT persists it. Cursors over a table or VALUES still read rows back
as first computed (RP-962 stays open for them).

#### Fixed

- A SCROLL cursor over `select nextval('s') from generate_series(1, 4)`
  answers `fetch 2`, `fetch backward 1`, `fetch first`, `fetch 2` with
  1 2, 3, 4, 5 6, as PostgreSQL 15.19 does (was 1 2, 1, 1, 2 3); FETCH
  ABSOLUTE, RELATIVE, FETCH 0 and MOVE BACKWARD ALL step the same rows
  PostgreSQL's executor does.
- A WITH HOLD cursor over such a query, fetched once, leaves `currval` at
  the row count after COMMIT and answers the rest from the rerun.

Fixes part of RP-962 (tests/backlog/test_rp_found_rp744.py::test_a_cursor_recomputes_a_volatile_column_when_it_reads_a_row_again, tests/backlog/test_rp_found_rp744.py::test_a_scroll_cursor_projects_each_row_its_executor_steps_onto)
