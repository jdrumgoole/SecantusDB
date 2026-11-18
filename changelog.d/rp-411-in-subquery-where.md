### An IN subquery with its own WHERE is planned as a join

`EXPLAIN` of `select * from e where b in (select b from k where a > 500)`
now prints the plan PostgreSQL makes of it: the subquery pulled up into a
join, its WHERE filtering the inner scan, priced, and hashed over a
`HashAggregate` where the inner side made unique is the cheaper path.
Before, a subquery with any WHERE of its own was left as written.

#### Fixed

- An IN subquery's WHERE over its own relation filters the semi join's
  inner scan, priced where that scan is sequential.
  Fixes RP-411, part (tests/backlog/test_rp_explain_costs.py::test_plan[an in subquery with its own where is planned as a join])
