### Rust PG server: EXPLAIN prices a hash join on two or more equalities

`EXPLAIN` of a two-table hash join on several column equalities
(`from e, k where e.b = k.b and e.a = k.a`, or the same as `JOIN ... ON`)
printed zeros. It now prints PostgreSQL 15.19's estimate: `cost_hashjoin`
pays each hash clause per built and per probed row, multiplies the clauses'
`eqjoinsel` selectivities, buckets on the smallest of their
`estimate_hash_bucket_stats`, and treats a side as `inner_unique` where any
of its keys is unique.

#### Fixed

- Fixes RP-374 (tests/backlog/test_rp_explain_costs.py::test_plan[a comma join's two equalities])
