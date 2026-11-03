### Rust PG server: EXPLAIN prices a WindowAgg of ranking functions

`EXPLAIN` of a `row_number()`, `rank()` or `dense_rank()` over an empty
window (`OVER ()`), straight over one relation's sequential scan, now
prints PostgreSQL 15's `cost_windowagg` estimate for the WindowAgg and its
Seq Scan, where both printed zeros.

#### Fixed

- `explain select a, row_number() over () from e` prints `WindowAgg
  (cost=0.00..27.50 rows=1000 width=12)` over `Seq Scan on e
  (cost=0.00..15.00 rows=1000 width=4)`, as PostgreSQL 15.19 does.
  A window with ORDER BY or PARTITION BY, and a window aggregate, are
  still unpriced (RP-347).

Fixes RP-323 (tests/backlog/test_rp_explain_costs.py::test_plan[windowagg])
