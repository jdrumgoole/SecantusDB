### The Rust PostgreSQL server: a three-table join under a WHERE over two of its tables

`a JOIN b ON ... JOIN c ON ... WHERE a.x < c.y` now returns its rows in the
order PostgreSQL 15.19 does. PostgreSQL applies that WHERE as a join filter
of the top join, the first one that holds both tables, and plans the join
order as without it. Before, any WHERE over more than one table turned the
join order search off.

#### Fixed

- `secantus-pg`: the three-table join search takes a WHERE whose
  multi-table conjuncts are not equalities and are first evaluable at the
  top join. The WHERE still runs over the joined rows, which keeps their
  order. A conjunct over a pair the search may join first is declined; its
  join filter's selectivity is not modelled. An equality between two tables
  is declined too, since it would join an equivalence class. 33 of 54
  shapes match 15.19 at 20 to 2,000 rows (3 before). Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_where_over_two_of_three_tables_is_planned`).
- `secantus-pgplan`: arithmetic (`+ - * / %`) counts as strict where
  `reduce_outer_joins` looks for what a WHERE proves non-NULL, as
  `find_nonnullable_rels` descends into a strict operator's arguments.

RP-028 stays open. A WHERE conjunct over a pair joined first is its new gap
test
(`tests/backlog/test_rust_postgres_gaps.py::test_a_where_over_a_pair_joined_first_is_planned`).
