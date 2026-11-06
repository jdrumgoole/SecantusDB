### Rust PG server: EXPLAIN ANALYZE counts the rows below a join

`EXPLAIN ANALYZE` of a join now prints each child's own `actual rows` and
`loops`, as PostgreSQL 15.19 does. Before, only the join itself was
counted.

#### Fixed

- A Nested Loop's parameterized inner index scan shows one loop per outer
  row, its rows per loop, and, as an Index Only Scan, the `Heap Fetches`
  summed over every loop (`Index Only Scan using k_pkey on k (actual
  rows=500 loops=1000)`, `Heap Fetches: 499500`).
- A Hash Join's Hash prints its table as `nodeHash.c` sizes and fills it:
  `Buckets: 1024  Batches: 1  Memory Usage: 47kB`, with `(originally N)`
  when the buckets grew while it was built. JSON, YAML and XML give the
  `Hash Buckets` ... `Peak Memory Usage` fields.
- A Nested Loop over a Materialize shows the Materialize rescanned per
  outer row, and an inner join's `Rows Removed by Join Filter`.

Fixes RP-386 (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a nested loop's children], tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a hash join's children]).
