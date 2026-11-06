### Rust PG server: a BETWEEN is two quals, as PostgreSQL plans it

PostgreSQL's parser turns `x BETWEEN lo AND hi` into `x >= lo AND x <= hi`
(`transformAExprBetween`), and the planner flattens that AND into the
scan's qual list. EXPLAIN now prints `where a between 15 and 55 and b <>
'v3'` as `Filter: ((a >= 15) AND (a <= 55) AND (b <> 'v3'::text))`, not
with a nested AND. The same holds for a joined scan's Filter, a
FROM-subquery's Filter and a composite index's `Index Cond`. A BETWEEN in
the WHERE over a LEFT join's nullable side now proves that side non-NULL,
as its two comparisons do, so the join is planned as the inner join
PostgreSQL makes of it. Before, it stayed a `Hash Left Join` with the
BETWEEN as a join Filter. Rows were right before and are unchanged.

#### Fixed

- A BETWEEN's two clauses are separate conjuncts wherever EXPLAIN splits
  a WHERE or an ON, and both count in `reduce_outer_joins`. Fixes RP-399
  (tests/backlog/test_rp_explain_costs.py::test_plan[a scan's between is two flat conjuncts],
  tests/backlog/test_rp_explain_costs.py::test_plan[a joined scan's between is two flat conjuncts],
  tests/backlog/test_rp_explain_costs.py::test_plan[a where between over a left join's nullable side makes it inner],
  tests/backlog/test_rp_explain_costs.py::test_plan[a composite index's between is two index conds],
  tests/backlog/test_rp_explain_costs.py::test_plan[a composite index's second column between]).
- Filed RP-409 (a comparison with a NULL constant is not folded to a false
  One-Time Filter, and alone prints an index the table does not have),
  RP-410 (a table whose only index is a composite one prices no scan of
  both its columns), RP-411 (an IN or EXISTS subquery is not a semi join
  in EXPLAIN) and RP-412 (a NOT over an AND, an OR or a BETWEEN is not
  pushed onto its arms), and added the OR and cast cases to RP-406.
