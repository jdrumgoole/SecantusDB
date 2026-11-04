### Rust PG server: EXPLAIN prices a Hash Join of two analysed tables

`EXPLAIN` of an inner join of two analysed tables on one column equality,
such as `select * from e join k on e.b = k.b`, now prints PostgreSQL 15.19's
cost, rows and width on the `Hash Join`, its `Hash` and both `Seq Scan`s.
Before this, every number in the plan was zero.

#### Fixed

- The join's rows come from `eqjoinsel_inner` over the two columns'
  most-common-value lists and null fractions. The hashed side's bucket
  size comes from `estimate_hash_bucket_stats`, which scales it up when
  the first most-common value is skewed. The cost is `cost_hashjoin`'s.
- The plan is priced only where PostgreSQL hashes the right table, as this
  server does. Where PostgreSQL's cost model hashes the left one, the plan
  still prints zeros (RP-359), as do a unique key, a WHERE and an outer
  join.
  Fixes RP-330 (tests/backlog/test_rp_explain_costs.py::test_plan[hash join],
  with `[hash join on a skewed column]` and `[hash join over nulls]`)
