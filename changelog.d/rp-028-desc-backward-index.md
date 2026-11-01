### Rust PG server: a DESC ORDER BY with an index in its order reads it backward, as PostgreSQL does

A join under a LIMIT with a descending ORDER BY on a join key that an index
returns in order is now searched instead of being joined whole and sorted.
As in `build_index_paths`, the index gives two paths: the forward scan, in
ascending order and still useful for merging, and a backward scan in the
query's order. A merge join that reads a side presorted takes that side's
direction. So the plan PostgreSQL picks (here a Nested Loop over an Index
Only Scan Backward) leaves the same ties.

#### Fixed

- `t0 JOIN t1 ON t0.b = t1.a ORDER BY t1.a DESC LIMIT 5`, with an index on
  `t1.a`, returns PostgreSQL 15.19's tied rows (6, 13, 20 rather than 20,
  13, 6).

Fixes RP-028, in part (tests/backlog/test_rust_postgres_gaps.py::test_a_descending_order_by_with_an_index_in_its_order)
