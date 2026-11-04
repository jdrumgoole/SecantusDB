### Rust PG server: EXPLAIN of a LEFT join its WHERE makes an inner one

`EXPLAIN` of a two-table LEFT join with a strict WHERE on its nullable side,
such as `select e.a from e left join k on e.b = k.b where k.a = 3`, now
prints PostgreSQL 15.19's plan: an inner `Hash Join`, priced as PostgreSQL
prices it, with the WHERE as a `Filter` on `k`'s scan under the Hash. Before
this it printed the WHERE as the join's top-level `Join Filter`, with every
cost zero.

#### Fixed

- The narrow two-table join path reduces a LEFT join to an inner one when
  a WHERE conjunct on its nullable side is a comparison, which no
  NULL-extended row can pass (`reduce_outer_joins`). It used to send that
  query to the general join planner. `NOT col` is not strict and still
  goes there. The rows returned are unchanged and match PostgreSQL's.
- EXPLAIN places a WHERE conjunct on a relation's scan when another
  conjunct proves that relation non-NULL (`find_nonnullable_rels`: a
  comparison over its column, or `IS NOT NULL`), since an outer join
  reduced this way no longer null-extends it.
- Over a table with an index, whose index scan PostgreSQL may pick, the
  plan is still unpriced (RP-354, RP-339).
  Fixes RP-365 (tests/backlog/test_rp_explain_costs.py::test_plan[a projected LEFT join reduced by its WHERE])
