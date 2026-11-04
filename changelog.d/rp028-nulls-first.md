### Rust PG server: a join's ORDER BY honours NULLS FIRST / NULLS LAST

An ORDER BY over a two-table join whose NULLs go against its direction's
default (`ASC NULLS FIRST`, `DESC NULLS LAST`) now puts the NULLs where the
clause says, and an ascending NULLS FIRST under a LIMIT leaves its ties as
PostgreSQL 15.19 does.

#### Fixed

- The two-table join path dropped an ORDER BY's `NULLS FIRST` /
  `NULLS LAST` and sorted by the direction's default, so a LEFT join's
  NULL-extended rows came out at the wrong end. It now hands such an ORDER
  BY to the general join planner, whose Sort honours it.
- The join search plans an ascending ORDER BY with its NULLs first under a
  LIMIT: an index scan's ascending order is of use for merging only, and a
  merge join on the ORDER BY's class sorts both sides NULLS FIRST
  (`select_outer_pathkeys_for_merge`), so the plan and its ties are
  PostgreSQL's.
- `tools/probes/pg_join_order_matrix.py --nulls-first` runs the join-order
  matrix with every ORDER BY ascending, NULLs first.

Fixes RP-028, part (tests/backlog/test_rust_postgres_gaps.py::test_an_ascending_nulls_first_order_by_keeps_its_ties,
tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_order_by_puts_its_nulls_where_it_says)
