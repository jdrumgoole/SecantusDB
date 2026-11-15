### A LEFT join the WHERE reduces to inner pushes its one-sided ON conjuncts

When a WHERE rejects the NULLs of a LEFT (or RIGHT) join's nullable side,
PostgreSQL's `reduce_outer_joins` makes the join an inner one before it
distributes the quals, so an ON conjunct over one side filters that side's
scan. The Rust PostgreSQL server now does the same rewrite first: the
conjunct leaves the Join Filter, the scan reads fewer rows, and the join
is ordered and priced as PostgreSQL's is.

#### Fixed

- `select * from e left join e e2 on e.a <> e2.a and e.a > 990 where
  e2.b = 'v1'` runs `(a > 990)` in `e`'s scan, under a Materialize, with
  `e e2` outer, as PostgreSQL 15 plans it.
  Fixes RP-902 (tests/backlog/test_rp_explain_costs.py::test_plan[a left join reduced to inner pushes its one-sided on conjunct])
