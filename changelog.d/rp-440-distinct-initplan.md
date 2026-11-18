### A SELECT DISTINCT above an InitPlan's scan is hashed and priced

On the Rust PostgreSQL server, `EXPLAIN` of a `SELECT DISTINCT` whose WHERE
compares a column with an uncorrelated scalar subquery now prints the
`HashAggregate` PostgreSQL 15 plans, its Group Key qualified and its cost,
rows and width estimated, where before it printed an unpriced `Unique`.

#### Fixed

- A SELECT DISTINCT of stored columns straight over an InitPlan's scan,
  on a table with no index, is planned as `create_distinct_paths`' hashed
  path where it beats a Sort and Unique, and priced by `cost_agg`.
  Trims RP-440 (tests/backlog/test_rp_explain_costs.py::test_plan[a distinct above an initplan's scan is priced]).
