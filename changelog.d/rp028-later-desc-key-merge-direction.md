### A descending key after the first one now steers the join plan as it does in PostgreSQL

Take an ORDER BY with a descending key after its first key, such as `ORDER BY t3.a DESC NULLS LAST, t0.c DESC`. PostgreSQL's `right_merge_direction` compares each join's sort order with every key of the query. So an ascending order on the second key's equivalence class is the wrong direction for merging, and it is no help to the ORDER BY either. PostgreSQL drops that order, and an index scan in that class is kept only as its backward scan. Until now the Rust PostgreSQL server's join order search did not know which direction a later key had. It reused an ascending merge's order in a merge above it without sorting again, a plan PostgreSQL never builds, and under a LIMIT rows tied on the first key came back in a different order. The later keys now carry their direction, and the search follows PostgreSQL's rule for them.

#### Fixed

- Rust PG server: `t0 JOIN t1 ON t0.c = t1.a JOIN t2 ON t0.b = t2.a LEFT JOIN t3 ON t0.c = t3.a ORDER BY t3.a DESC NULLS LAST, t0.c DESC LIMIT 5` returns PostgreSQL's rows. Across the join-order matrix's 810 shapes, every ORDER BY variant with a descending later key now matches PostgreSQL 15.20 (793, 794 and 809 before, depending on the variant) (RP-028, part).

Fixes RP-028, part (tests/backlog/test_rust_postgres_gaps.py::test_a_four_table_star_left_join_desc_nulls_last_then_desc)
