### A join's ascending order under a descending ORDER BY on its class is of no use

When an ORDER BY's first key is descending and other keys follow it, PostgreSQL's merge joins on that key's equivalence class sort ascending. The path they leave is then in the wrong direction for any merge above (`right_merge_direction`) and of no use for the ORDER BY, so `build_join_pathkeys` truncates it to no order, and a merge above sorts that side again. The Rust PostgreSQL server kept the ascending order as of use for merging and read it presorted in the merge above, which made a plan PostgreSQL never builds look cheaper, and ties under the later keys came back in another order. That order is now discarded the way PostgreSQL discards it.

#### Fixed

- Rust PG server: a five-table join of two hubs, `t0 JOIN t1 ON t0.b = t1.a JOIN t2 ON t0.c = t2.a JOIN t3 ON t1.b = t3.a JOIN t4 ON t1.b = t4.a ORDER BY t4.a DESC, t0.c LIMIT 5`, returns PostgreSQL's rows (RP-028, part).

Fixes RP-028, part (tests/backlog/test_rust_postgres_gaps.py::test_a_five_table_two_hub_join_desc_with_another_key)
