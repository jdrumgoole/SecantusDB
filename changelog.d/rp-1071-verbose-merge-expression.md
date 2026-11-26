### Rust PG server: EXPLAIN VERBOSE of a merge join on an expression key prints every node's Output

`explain verbose select * from e join e e2 on e.b = e2.b || 'x'` printed
its Merge Join as plain EXPLAIN does. It now prints PostgreSQL's target
lists: `Output` on the join, each Sort and each scan, the scans named
`public.e`, and the expression side carrying its key as the resjunk entry
`prepare_sort_from_pathkeys` adds (`Output: e2.a, e2.b, ((e2.b ||
'x'::text))` on the Sort over `Output: e2.a, e2.b, (e2.b || 'x'::text)` on
the scan).

#### Fixed

- `EXPLAIN VERBOSE` of a two-relation merge join whose key is an
  expression of one side prints the target lists PostgreSQL 15 prints,
  with a narrower select list and with a WHERE on the column side too.

Fixes RP-1071 (tests/backlog/test_rp_explain_costs.py::test_plan[verbose of a merge join on an expression key])
