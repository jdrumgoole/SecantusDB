### Rust PG server: EXPLAIN splits a join's equality from its inequalities (RP-375)

An inner join whose ON (or comma WHERE) ANDs a column equality with `<`,
`<=`, `>` or `>=` conjuncts now prints and prices as PostgreSQL 15.19
does: only the equality is the `Hash Cond`, the rest the join's `Join
Filter`, cheapest first.

#### Fixed

- `explain select * from e join k on e.b = k.b and e.a < k.a` printed
  `Hash Cond: ((e.b = k.b) AND (e.a < k.a))` with zero costs; it is now
  `Hash Join (cost=27.50..1420.00 rows=33333 width=14)` with `Hash Cond:
  (e.b = k.b)` and `Join Filter: (e.a < k.a)`. Each filter operator is
  charged on every pair the hash clause matches, and each inequality
  scales the rows by `DEFAULT_INEQ_SEL`. The comma form, `from e, k
  where ...`, plans alike.

Fixes RP-375 (tests/backlog/test_rp_explain_costs.py::test_plan[a join's equality with an inequality beside it]).

A LEFT join's, or a `<>`'s, filter beside the equality now prints the
same split but is still unpriced: filed as RP-379.
