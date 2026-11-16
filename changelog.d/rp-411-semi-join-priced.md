### A hash semi join is priced as PostgreSQL prices it

EXPLAIN of an `IN`, `= any` or `EXISTS` subquery planned as a `Hash Semi
Join` now prints PostgreSQL's estimates instead of zeros: the outer match
fraction from `eqjoinsel_semi` (its most-common-values branch included),
and `final_cost_hashjoin`'s semi-join costing, where each outer row's probe
stops at its first match. The estimate is given only where the semi join
beats the inner join of the unique-ified subquery, which is priced for the
comparison, so a plan PostgreSQL would draw differently is not passed off
as its answer.

#### Fixed

- Rust PG server: a `Hash Semi Join` and its inputs carry PostgreSQL
  15.19's costs, rows and widths.
  Fixes RP-411, part (tests/backlog/test_rp_explain_costs.py::test_plan[a hash semi join is priced])
