### Rust PG server: EXPLAIN prices a window function's FILTER and expression argument

A WindowAgg whose function has a `FILTER (WHERE ...)` clause, or whose
argument is an expression rather than a plain column, now prints
PostgreSQL's estimate instead of zeros: `cost_windowagg` adds each
operator those expressions evaluate per input row, and the scan below
returns the columns they read.

#### Fixed

- `explain select a, sum(a) filter (where a > 5) over () from e` and
  `explain select a, sum(a+1) over () from e` both print `WindowAgg
  (cost=0.00..30.00 rows=1000 width=12)`, as PostgreSQL 15.19 does.
  Fixes RP-352 (tests/backlog/test_rp_explain_costs.py::test_plan[an aggregate under a filter clause], tests/backlog/test_rp_explain_costs.py::test_plan[an aggregate of an expression]).
  Stacked windows, the item's third case, are split out as RP-355.
