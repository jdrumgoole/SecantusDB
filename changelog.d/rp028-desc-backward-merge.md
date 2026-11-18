### A descending ORDER BY of several keys over a join merges on the index's backward scan

PostgreSQL merges a join on an index's backward scan when the ORDER BY's first key is descending and keys follow it, leaving the rows in the query's order for an Incremental Sort; and a LEFT join ordered by its preserved key reads the nullable side's forward scan presorted under a Merge Right Join. The Rust PostgreSQL server weighed neither path, so ties under the later keys came back in another order. Both are now planned as PostgreSQL plans them.

#### Fixed

- Rust PG server: `t0 [LEFT] JOIN t1 ON t0.b = t1.a ORDER BY t0.b DESC, t0.c LIMIT n` with an index on `t1.a` returns PostgreSQL's rows (RP-028, part).

Fixes RP-028, part (tests/backlog/test_rust_postgres_gaps.py::test_a_two_table_join_desc_with_another_key_at_3000_rows, tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_desc_with_another_key_merges_on_the_forward_scan)
