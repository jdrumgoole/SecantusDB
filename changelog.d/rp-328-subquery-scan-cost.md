### Rust PG server: EXPLAIN prices a Subquery Scan with a WHERE

`EXPLAIN` of a WHERE over a FROM-subquery that is a LIMIT of one table now
prints PostgreSQL 15's estimate on the Subquery Scan, its Limit and the
Seq Scan below, and the Subquery Scan's `Filter:` line, qualified by the
subquery's alias as PostgreSQL prints it.

#### Fixed

- `explain select * from (select a from e limit 3) s where a > 1` printed
  zeros and no Filter line; it is now `Subquery Scan on s
  (cost=0.00..0.08 rows=2 width=4)` with `Filter: (s.a > 1)`, as on
  PostgreSQL 15.19. The cost is `cost_subqueryscan` over the Limit; the
  rows read the base column's statistics through the subquery, a distinct
  count stored as a fraction scaling to the subquery's rows.

Fixes RP-328 (tests/backlog/test_rp_explain_costs.py::test_plan[subquery scan])
