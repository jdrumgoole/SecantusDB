### A cursor reads a set operation under a UNION ALL when it reaches it

A cursor over a `UNION ALL` whose branch is itself a `UNION`, `INTERSECT`
or `EXCEPT` used to run the whole query at its first FETCH, so an error in
a later row of the first branch was raised before any row was returned.
That set operation is now one more branch of the Append, run only when the
cursor reaches it, as PostgreSQL does.

#### Fixed

- Rust PG server: a set operation of another kind under a cursor's
  `UNION ALL` is read as one more leaf of the Append.
  Fixes RP-896 (tests/backlog/test_rp_found_rp744.py::test_a_cursor_over_a_union_under_a_union_all_computes_its_rows_as_they_are_fetched)
