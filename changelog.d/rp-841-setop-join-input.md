### An INTERSECT over a join hashes the smaller input first

`select a.x from t692j a join t692j b on a.k = b.k intersect select 1.000` returned `1.0` on the Rust PostgreSQL server, where PostgreSQL 15.19 returns `1.000`. A HashSetOp keeps the row its first input gives for each group, and PostgreSQL puts the input with fewer estimated groups first. The server could not estimate a join input, so it kept the written order. It now estimates an inner join of two tables on one equality as PostgreSQL does. Over tables never analysed, the rows come from `eqjoinsel` with the default distinct counts. Over analysed tables, they are the priced hash join's rows. The groups are `estimate_num_groups` clamped to the join's rows, so the one-row constant goes first and its row is the group's.

#### Fixed

- An INTERSECT or EXCEPT with an inner join of two tables as an input orders its inputs, and so picks each group's row, as PostgreSQL 15.19 does.

Fixes RP-841 in part (tests/backlog/test_rp_found_rp692.py::test_an_intersect_over_a_join_puts_the_smaller_input_first, tests/backlog/test_rp_found_rp692.py::test_an_intersect_over_a_join_with_a_unique_key_puts_the_smaller_input_first)
