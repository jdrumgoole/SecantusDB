### EXPLAIN prints the Seq Scan PostgreSQL picks over an index the server reads

When a committed table's range is wide enough that PostgreSQL 15 prefers the
heap, as in `select * from k2 where y > 70` over a weakly correlated index on
`y`, EXPLAIN on the Rust PostgreSQL server printed the index scan its storage
reads, with zero costs. It now prints PostgreSQL's choice:
`Seq Scan on k2 (cost=0.00..17.50 rows=290 width=8)` with `Filter: (y > 70)`.
The cost model already chose the sequential scan. EXPLAIN now replaces the
storage's node with it instead of leaving that node unpriced.

#### Fixed

- EXPLAIN of one table's indexed scan prints the Seq Scan when `set_cheapest`
  prefers it (`user_scan_path_reading`).
- Fixes RP-926 (tests/backlog/test_rp_explain_costs.py::test_a_wide_range_over_a_weakly_correlated_index)
