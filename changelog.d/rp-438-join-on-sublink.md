### Rust PG server: a subquery in a JOIN's ON runs

A join condition holding a subquery now runs, as PostgreSQL 15.19 runs
it: `select ... from s join k on s.a = k.a and k.b = (select 'p'::text)`,
the same under a LEFT, RIGHT or FULL join, and an `IN`, `NOT IN`,
`EXISTS`, `ANY` or `ALL` subquery in the ON. Before, each answered
`0A000 SubLink is not supported yet`, though the same subquery in the
WHERE ran.

#### Fixed

- The subqueries in a join's ON are resolved as a WHERE's are. An
  uncorrelated one runs once, while the plan that executes is made. A
  volatile one (`(select nextval('s'))`) runs once per execution and not
  at all under `EXPLAIN`. A correlated one, reading either side of the
  join (`k.bc = (select max(bc) from k k2 where k2.a = s.a)`), is
  evaluated per joined row. This holds in a join nested inside another
  join too. 26 shapes compared row for row with PostgreSQL 15.19, with
  parameters and errors included.
- Fixes RP-438
  (tests/backlog/test_rp_join_on_sublink.py::test_subquery_in_join_on),
  with the crate test `a_join_on_may_hold_a_subquery` beside it.
- Filed RP-440: `EXPLAIN` prints an uncorrelated subquery inline, with no
  `InitPlan` and no estimate, in a WHERE as in a join's ON. In an ON the
  conjunct also stays in the `Join Filter` where PostgreSQL filters the
  scan.
