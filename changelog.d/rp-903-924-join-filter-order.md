### A nested loop's Join Filter is flattened and cheapest first

A nested loop's Join Filter now prints its ON clause the way PostgreSQL's
planner leaves it: a BETWEEN becomes its two comparisons in the one flat
qual list, and the conjuncts are ordered cheapest first, as
`order_qual_clauses` sorts them, with a cast of a non-string column to text
costing its output and input functions.

#### Fixed

- `EXPLAIN` of a nested loop prints `((e.a <> e2.a) AND (e.a >= 10) AND
  (e.a <= 20))` for a BETWEEN in the ON, not a nested AND.
  Fixes RP-903 (tests/backlog/test_rp_explain_costs.py::test_plan[a between among a join's on conjuncts is flattened])
- A nested loop's Join Filter is in cost order, not written order.
  Fixes RP-924 (tests/backlog/test_rp_explain_costs.py::test_plan[a nested loop's join filter in cost order])
