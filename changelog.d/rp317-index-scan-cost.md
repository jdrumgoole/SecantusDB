### Rust PG server: EXPLAIN prices a user table's Index Scan

A plan that is one user table's plain Index Scan now prints PostgreSQL
15.19's estimate -- `cost_index` over `btcostestimate`, with the leading
column's correlation -- instead of `cost=0.00..0.00 rows=0 width=0`.

#### Fixed

- `explain select * from k where a = 5` over an analysed primary key is
  `Index Scan using k_pkey on k  (cost=0.28..8.29 rows=1 width=7)`, as on
  PostgreSQL. A scan the model does not price (an index-only scan, a range
  or `OR` on the key, a later key column, two serving indexes) still prints
  zeros. Fixes RP-317
  (tests/backlog/test_rp_explain_costs.py::test_plan[index scan]).
