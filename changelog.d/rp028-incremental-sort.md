### Rust PG server: an ORDER BY of several columns over a join sorts incrementally, as PostgreSQL's does

`t0 JOIN t1 ON t0.b = t1.a ORDER BY t1.a, t0.c LIMIT 5` now leaves its ties
as PostgreSQL 15 does. PostgreSQL runs that ORDER BY as an Incremental Sort
over a merge join already in `t1.a`'s order, sorting each group of equal
`t1.a` by `t0.c` alone, and the order those groups' ties come out in is
that node's. The join order search now weighs the same plans PostgreSQL's
`create_ordered_paths` does for an ORDER BY of several plain columns under
a LIMIT: every path in the first column's order under an Incremental Sort,
priced as `cost_incremental_sort` prices it, beside a bounded Sort of the
cheapest path. The plan it picks is run as `nodeIncrementalSort.c` runs
it, in batches of at least 32 rows sorted by every key, with a large group
moved to a sort by the later keys alone.

On the join-order matrix with a second ORDER BY key, every one of 810
shapes of two to five tables now matches PostgreSQL 15.19, against 676 to
778 before. A first key that is descending or NULLS FIRST, followed by
another key, is still declined and planned as before.

#### Fixed

- `secantus-pg` `tuplesort::incremental_sort`: PostgreSQL's Incremental Sort
  executor, full-sort and presorted-prefix modes, bounded by the LIMIT.
- `secantus-pgplan` `joincost::cost_incremental_sort`, and
  `ordered_limit_choice` weighing it beside the bounded Sort.
- `secantus-pg` join search: an ORDER BY of several columns under a LIMIT is
  searched, a later key in the first key's equivalence class dropped as a
  redundant pathkey; a LEFT join with such an ORDER BY is no longer
  streamed.

Fixes RP-028, part (tests/backlog/test_rust_postgres_gaps.py::test_a_two_column_order_by_over_a_join_keeps_its_ties, tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_order_by_two_columns_sorts_incrementally, tests/backlog/test_rust_postgres_gaps.py::test_an_order_by_key_in_the_first_keys_class_is_redundant).
