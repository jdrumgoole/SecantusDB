### A join narrowed to a few rows probes the unique key's index

When a WHERE range on one side of a join leaves only a few rows, PostgreSQL
probes the other table's unique key index once per row instead of hashing the
whole table. The Rust PostgreSQL server's EXPLAIN now draws and prices that
plan as PostgreSQL 15.19 does (`select * from e join k on k.a = e.a where e.a
> 998` is a `Nested Loop` into `Index Scan using k_pkey`, `Index Cond: (a =
e.a)`), where it printed an unpriced hash join. EXPLAIN ANALYZE counts its
rows and loops, and EXPLAIN VERBOSE says its inner side is unique.

#### Fixed

- EXPLAIN plans and prices an inner join whose unique-key side is cheapest
  probed through its index per outer row.
- EXPLAIN VERBOSE printed no `Inner Unique: true` for a join with no qual
  after its `Output`.
  Fixes RP-978 in part (tests/backlog/test_rp_explain_costs.py::test_plan[a range on the join key of a unique inner is a parameterized nested loop])
