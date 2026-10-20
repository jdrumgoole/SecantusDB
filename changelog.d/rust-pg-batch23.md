### Rust PostgreSQL server: the join order search takes any tree of INNER and LEFT joins

A join written with parentheses -- `a JOIN (b JOIN c)`, `a LEFT JOIN (b JOIN
c)`, `(a JOIN b) JOIN (c JOIN d)` -- or a RIGHT join over a join, which
PostgreSQL flips into a LEFT join whose nullable side is a join, was not a
left-deep chain, so the join order search declined it. It came back in the
general path's order rather than PostgreSQL's. The search now walks any binary
tree of INNER and LEFT joins as `deconstruct_jointree` does: tables left to
right, ON clauses bottom up.

#### Fixed

- `secantus-pg` (`join_order.rs`): `four_table_chain` walks the whole tree
  (`walk_tree`). A LEFT join whose nullable side is an inner join of tables
  takes that side whole as its `min_righthand` (`make_outerjoininfo`).
  `reduce_outer_joins` runs top down: an inner join passes its tables down
  both sides, a LEFT join passes what is above it to its left side only.
  Three tables are searched when the tree is bushy (a left-deep three stays
  `three_table_plan`'s), four to eight in any shape. Fixes part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_right_join_over_a_join_is_searched_as_its_left_join`,
  now a regression test). Measured against PostgreSQL 15.19: bushy trees of
  three to five tables 21 of 24 probes at 20 and 300 rows (none before),
  RIGHT joins 27 of 33 (24 before). Every left-deep four- to eight-table
  shape is unchanged.

#### Added

- RP-028's open gap is a LEFT join whose nullable side holds another LEFT
  join, which `make_outerjoininfo` widens by the lower join's own sides and
  the search declines:
  `tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_whose_nullable_side_holds_a_left_join_is_searched`.
