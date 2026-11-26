### An IN subquery comparing expressions is planned as a join

`EXPLAIN` of `select * from e where a::bigint in (select a::bigint + 0
from k where b = 'v3')` now prints the plan PostgreSQL 15 makes of it: the
subquery pulled up into a join hashed on the two expressions, over a
`HashAggregate` grouped by the inner expression where the inner side made
unique is the cheaper path, priced. Before, an IN whose either side was
anything but a bare column was left as written, an unpriced scan with
`Filter: (...)`.

#### Fixed

- Either side of an IN subquery may be an expression over one column of
  its relation (casts, arithmetic, string operators, built-in calls): its
  Hash Cond and Group Key print the expressions, and it is priced with no
  statistics for them and the column's groups for the aggregate.
  Fixes RP-411, part (tests/backlog/test_rp_explain_costs.py::test_plan[an in subquery comparing expressions is planned as a join])
- A semi join's inner distinct count clamped to the inner side's rows is
  no longer taken as the default, as `eqjoinsel_semi` clamps it: `a in
  (select a + 0 from k where b = 'v3')` estimates 100 rows, not 500.
  Fixes RP-411, part (tests/backlog/test_rp_explain_costs.py::test_plan[an in subquery of an expression over a filtered side clamps its distinct count])
- An IN subquery's WHERE that names its relation by its alias (`select a
  from e e2 where e2.b = 'v3'`) filters the inner scan of the semi join,
  as the bare column does.
  Fixes RP-411, part (tests/backlog/test_rp_explain_costs.py::test_plan[an in subquery whose where names its alias is a hash semi join])
