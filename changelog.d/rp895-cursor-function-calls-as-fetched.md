### The Rust PostgreSQL server: a cursor calls a function only for the rows it fetches

A user function, a `nextval` or a correlated subquery in a cursor's
select list is now computed for the rows a FETCH reaches, and no others.
Before, the server computed these columns for every row when the
cursor's query ran. A row no FETCH reached could raise its error, and a
sequence was drawn once per row of the whole result. Now `declare c cursor
for select nextval('s') from generate_series(1, 3)` followed by `fetch 1`
draws one value, as on PostgreSQL 15.19.

Such a column also reads the snapshot that PostgreSQL gives it. A STABLE
function reads what the DECLARE's snapshot holds, so a row the block
inserts after the DECLARE is not seen. A VOLATILE function takes a
snapshot of its own and sees it.

#### Fixed

- `secantus-pg`: while a cursor's query runs (`cursor_run`),
  `project_rows_into` leaves a column that runs through the executor's
  hooks to the row stream. The stream is pulled in the block's
  transaction under `with_executor_hooks`. A cursor whose select list
  reads the database (`correlated::reads_portal_snapshot`: a correlated
  subquery, a non-volatile user or executor function, a user cast)
  computes its remaining rows before another statement of the block moves
  the snapshot. An error there is kept for the FETCH that reaches it.
  Fixes RP-895 (tests/backlog/test_rp_found_rp744.py::test_a_cursor_computes_a_function_call_as_it_is_fetched;
  regression test `test_a_cursors_function_reads_the_snapshot_its_volatility_gives`
  in the same file).

#### Known gaps

- RP-962 filed: a row a cursor reads again (a backward FETCH, a WITH HOLD
  cursor persisted at COMMIT) is not computed again, where PostgreSQL
  recomputes a volatile column.
- RP-963 filed: `select * from cursor_to_xml(...)` reads the cursor twice.
