### Rust PG server: a LEFT join's DESC NULLS LAST merges on the nullable key's backward index scan

`t0 LEFT JOIN t1 ON t0.b = t1.a ORDER BY t1.a DESC NULLS LAST LIMIT n`, with
an index on `t1.a`, now leaves its ties as PostgreSQL does: the join search
merges on the index's backward scan, whose DESC NULLS FIRST order is the
right direction for merging (`right_merge_direction` weighs the direction
only) though not the ORDER BY's own pathkey, and sorts above the merge.

#### Fixed

Fixes RP-028, part (tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_descending_nulls_last_over_an_index_reads_it_backward).
