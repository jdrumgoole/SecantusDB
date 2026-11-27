### Rust PG server: `INSERT ... SELECT` checks a set operation's types before its columns

`insert into tct(c) select null::text union select 1` now fails with 42804
`UNION types text and integer cannot be matched` at the `1`, as PostgreSQL
15 does: parse analysis resolves the query's set operation before the
`INSERT` assigns its columns. Before, the server checked the column first
and reported `column "c" is of type ctp but expression is of type text`.

#### Fixed

- An `INSERT ... SELECT` over `UNION` / `INTERSECT` / `EXCEPT` resolves the
  set operation's column types without running either side: the left
  subtree first, then the right, then each column. Unmatched types (42804),
  a side that does not convert (42846) and a column-count mismatch (42601)
  are reported before the target columns are checked, at the positions
  PostgreSQL gives them.

Fixes RP-1088 (tests/backlog/test_rp_found_rp1050.py::test_insert_select_set_op_types_before_assignment), with the regression test tests/test_rust_pgserver_slice.py::test_insert_select_set_operation_types_come_first.
