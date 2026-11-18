### An IN subquery read through an index is planned as PostgreSQL plans it

When an IN subquery's own WHERE clause bounds an index, the PostgreSQL server's
EXPLAIN now shows the plan PostgreSQL 15.19 picks. That plan is a Hash Join
over a HashAggregate of the Index Scan, costed to the cent. Previously it
printed a zero-cost Hash Semi Join over a sequential scan. Pricing the
unique-ified inner side also became more faithful. Its group count is now
scaled to the rows the subquery keeps, and its hash buckets hold one entry
each, as PostgreSQL's planner assumes for a unique path.

#### Fixed

- `explain select * from e where b in (select b from k where a > 990)` now
  prints `Hash Join (cost=8.61..37.36 rows=1000)` over
  `HashAggregate` over `Index Scan using k_pkey`, as 15.19 does.
  Partly fixes RP-411 (tests/backlog/test_rp_explain_costs.py::test_plan[an in subquery read through an index is planned as a join]).
