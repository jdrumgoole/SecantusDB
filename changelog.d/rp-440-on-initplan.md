### A scalar subquery in a join's ON is an InitPlan on its side's scan

EXPLAIN of a join whose ON compares one side's column with an
uncorrelated scalar subquery, such as `e left join k on e.a = k.a and
k.b = (select 'v3'::text)`, now prints the plan PostgreSQL makes: the
subquery is `InitPlan 1 (returns $0)` on the join, and the comparison runs
in that side's scan as `Filter: (b = $0)` rather than in the join's
`Join Filter`. Inner and LEFT joins both place it there.

#### Fixed

- Rust PG server: an ON conjunct comparing a column with a FROM-less
  scalar subquery is an InitPlan placed in its side's scan.
  Fixes RP-440, part (tests/backlog/test_rp_explain_costs.py::test_plan[a join's on subquery is an initplan on its scan])
