### A cursor over a UNION ALL converts each branch as it reads it

A cursor over a `UNION ALL` whose branches have different column types
(`select 1/(k - 1) ... union all select 2.5`, an untyped literal beside a
`varchar`, a `float4` beside a `float8`) now reads its branches in order,
converting each row to the set operation's unified types as it is fetched,
as PostgreSQL's Append does. Before, such a cursor ran its whole query at
the first FETCH, so a later row's error was raised before an earlier row
was returned.

#### Fixed

- Rust PG server: a cursor's Append converts a branch's rows to the unified
  types as they are read, and describes the unified types rather than the
  left branch's. Fixes RP-896 in part
  (tests/backlog/test_rp_found_rp744.py::test_a_cursor_over_a_converting_union_computes_its_rows_as_they_are_fetched,
  tests/backlog/test_rp_found_rp744.py::test_a_union_all_cursor_converts_each_branch_to_the_unified_types);
  a UNION, INTERSECT or EXCEPT under the UNION ALL still runs whole.
