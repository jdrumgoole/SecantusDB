### Rust PG server: EXPLAIN prices a WindowAgg over a Sort

A window with PARTITION BY or ORDER BY now shows the Sort PostgreSQL puts
below its WindowAgg, and both carry PostgreSQL 15's estimates.

#### Fixed

- `explain select a, row_number() over (order by a) from e` printed a
  `WindowAgg` straight over the `Seq Scan`, both at
  `cost=0.00..0.00 rows=0 width=0`. It now prints `WindowAgg
  (cost=64.83..82.33 rows=1000 width=12)` over `Sort (cost=64.83..67.33
  ...)` with `Sort Key: a`, as PostgreSQL 15.19 does: `cost_sort` by the
  partition columns, then the order columns (with `DESC` and `NULLS
  FIRST` / `NULLS LAST` printed where they differ from the default), and
  `cost_windowagg`'s one comparison per key column per row. This covers
  `row_number`, `rank` and `dense_rank` sharing one window on a table with
  no index.
- Aggregates over a window, window functions with an argument, and
  several distinct windows in one SELECT remain open as RP-351.

Fixes RP-347 (tests/backlog/test_rp_explain_costs.py::test_plan[a windowagg over a sort])
