### EXPLAIN shows the anti join a LEFT join's IS NULL makes

A LEFT join whose WHERE keeps only the rows with no match -- `left join e2
on e.a = e2.a where e2.a is null` -- is an anti join to PostgreSQL's
planner: it stops looking at the first match and drops the `IS NULL`
check. The Rust PostgreSQL server now explains it that way, as a `Hash
Anti Join` over an equality or a `Nested Loop Anti Join` over a `<>`, with
PostgreSQL's own costs and row estimate, where it printed a `Left Join`
with a `Filter` above it and zero costs. The rows returned were already
right.

#### Fixed

- EXPLAIN of a LEFT join whose WHERE tests the nullable side's join key
  `IS NULL` prints PostgreSQL 15's `Hash Anti Join` / `Nested Loop Anti
  Join`, priced by `final_cost_hashjoin` / `final_cost_nestloop`'s
  JOIN_ANTI arms over `compute_semi_anti_join_factors`
  (`eqjoinsel_semi`, and `neqjoinsel` for JOIN_ANTI).
  Fixes RP-908 (tests/backlog/test_rp_explain_costs.py::test_plan[a left join where the nullable side is null is an anti join], tests/backlog/test_rp_explain_costs.py::test_plan[a left join on an equality where the nullable side is null is an anti join])
- Filed RP-980 (NOT EXISTS as a Hash Anti Join) and RP-981 (an anti join on
  an expression key).
