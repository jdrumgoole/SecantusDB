### Rust PG server: a DESC ORDER BY of a LEFT join's nullable column keeps PostgreSQL's ties

Under a LIMIT, `ORDER BY t2.a DESC` where `t2` is the nullable side of a
LEFT join now returns the rows PostgreSQL returns, ties included. The join
order search sorts the merge with that side outer descending, as
`select_outer_pathkeys_for_merge` does. An input already in ascending order
gets a Sort for that merge, not a presorted read.

#### Fixed

- A DESC NULLS FIRST ORDER BY of a NULL-extended key under a LIMIT, over
  three to five tables: 1,130 of 1,140 shapes of the join order matrix
  match PostgreSQL 15.19 (1,097 before). Every shape left has an index.
- `tools/probes/pg_join_order_matrix.py --desc` runs the matrix with every
  ORDER BY descending.

Fixes RP-028, part (tests/backlog/test_rust_postgres_gaps.py::test_a_descending_order_by_of_a_nullable_key_under_a_limit, tests/backlog/test_rust_postgres_gaps.py::test_a_descending_nullable_key_beside_an_inner_class_keeps_its_ties)
