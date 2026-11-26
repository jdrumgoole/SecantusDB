### EXPLAIN prints the Sort of an ORDER BY over a two-table join, priced

`secantusd-pg` plans a two-table join with a one-column ORDER BY on its
narrow join path, which sorts the joined rows but whose EXPLAIN drew the
Hash Join alone. EXPLAIN now prints the `Sort` with its `Sort Key` above the
join, as PostgreSQL 15 does. Where no path already returns the rows in that
order, it is priced as PostgreSQL prices it.

#### Fixed

- `explain select e.b, k.b from e join k on e.a = k.a order by k.a` printed
  the Hash Join with no Sort above it. It now prints `Sort` / `Sort Key:
  k.a` over the join, `DESC` included, in every format.
- Where the ORDER BY column is not a join key and no index of its table
  leads with it, the Sort and the join below it carry PostgreSQL's
  estimates, where both were `cost=0.00..0.00 rows=0 width=0`. The Sort is
  `cost_sort` over the join's cheapest path, and the join's width counts
  the ORDER BY column when the select list does not take it. `EXPLAIN
  ANALYZE` prints the join's own counts under the Sort.
- Fixes RP-1056 (`tests/backlog/test_rp_explain_costs.py::test_plan[an order by over a join prints its sort]`,
  `test_plan[an order by over a join prices its sort]` and three more).
  Files RP-1119: the Sort stays unpriced when it orders by a join key,
  when the planner keeps the ORDER BY as joined rows (an output alias, an
  ordinal, a comma join), and when a WHERE fixes a join key.
