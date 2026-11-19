### A Sort under a select list that calls a function is priced

`EXPLAIN select upper(b) from e order by lower(b)` now prints PostgreSQL's
costs, `Sort (cost=69.83..72.33 rows=1000 width=64)` over a `Seq Scan`
whose target list carries the call at its `procost`, where it printed
`cost=0.00..0.00`. A select-list entry calling a non-volatile built-in, over
the column alone or inside an operator expression, is priced as
`cost_qual_eval` prices a FuncExpr; one costly enough that
`make_sort_input_target` could postpone it above the Sort stays unpriced.

#### Fixed

- Rust PG server: a computed select-list entry that calls a built-in is
  priced in the target list of the scan it is computed in.
  Fixes RP-1036 (tests/backlog/test_rp_explain_costs.py::test_plan[a sort under a select list calling a function is priced])
