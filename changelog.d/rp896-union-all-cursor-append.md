### The Rust PostgreSQL server: a cursor over a UNION ALL reads its branches in order

A cursor over a `UNION ALL` now reads its branches one after another, as
PostgreSQL's Append does. A branch runs only when the FETCHes have read
everything before it, and each branch's select list is computed for the
rows a FETCH reaches. Before, the whole set operation was computed at
once. A row no FETCH reached could raise its error. Now `declare c cursor
for select 1/(k - 1) from (values (2), (1)) s(k) union all select 5`
followed by `fetch 1` answers `1`, as on PostgreSQL 15.19.

This holds when every branch's columns already have the set operation's
types. A branch whose columns unification would convert, such as an
integer branch beside a numeric one, still makes the whole set operation
run at the first FETCH.

#### Fixed

- `secantus-pg`: `PendingRows::Append` holds a cursor's UNION ALL leaves
  (`PgHandler::append_leaves`: no ORDER BY, LIMIT or OFFSET over it, no
  other set operation under it, every leaf's described column types the
  same). Each leaf runs when the one before it ends, or before another
  statement of the block moves the snapshot. A failed leaf keeps its error
  for the FETCH that reaches it. Partly fixes RP-896
  (tests/backlog/test_rp_found_rp744.py::test_a_cursor_over_a_union_computes_its_rows_as_they_are_fetched;
  regression test `test_a_union_all_cursor_reads_its_branches_in_order`
  in the same file). The item stays open for a branch that unification
  converts.
