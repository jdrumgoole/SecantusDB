### The Rust PostgreSQL server: a LEFT join inside another's nullable side is planned as PostgreSQL plans it

A query such as `a LEFT JOIN (b LEFT JOIN c ON ...) ON ...` now returns its
rows in the order PostgreSQL 15.19 does. Before, the join order search
declined it, and the rows came out in the general path's order.

#### Fixed

- `secantus-pg`: the join order search takes a LEFT join whose nullable side
  holds another LEFT join. Each outer join's minimum sides are widened by a
  lower outer join's as `make_outerjoininfo` widens them, and a LEFT join's
  ON clause makes the tables it reads non-null inside its nullable side, as
  in `reduce_outer_joins`. 18 of 20 nested shapes of three and four tables
  match 15.19 at 20 and 300 rows. The other two mix an equivalence class
  with a LEFT join, which RP-028 still lists. Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_whose_nullable_side_holds_a_left_join_is_searched`);
  FULL joins stay open
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_full_join_is_planned`).
