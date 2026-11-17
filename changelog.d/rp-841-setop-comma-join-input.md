### An INTERSECT over a comma join hashes the smaller input first

`select a.x from t692j a, t692j b where a.k = b.k intersect select 1.000` returned `1.0` on the Rust PostgreSQL server, where PostgreSQL 15.19 returns `1.000`, as it already did for the same join written with ON. PostgreSQL makes an inner join's WHERE and ON clauses one list, so it estimates the comma form exactly as the ON form and puts the one-row constant first. The server now reads the comma join's WHERE equality as its join clause when it estimates a set operation's input.

#### Fixed

- An INTERSECT or EXCEPT whose input is a comma join of two tables on one WHERE equality orders its inputs, and so picks each group's row, as PostgreSQL 15.19 does.

Fixes RP-841 in part (tests/backlog/test_rp_found_rp692.py::test_an_intersect_over_a_comma_join_puts_the_smaller_input_first)
