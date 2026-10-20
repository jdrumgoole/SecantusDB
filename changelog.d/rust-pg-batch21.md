### Rust PostgreSQL server: a RIGHT join is planned as the LEFT join it is

PostgreSQL never plans a RIGHT join as written: `reduce_outer_joins` flips
it into the LEFT join with its sides swapped, and the planner only ever sees
LEFT. The Rust server kept the RIGHT join, which no cost-based path took, so
`a RIGHT JOIN b` returned PostgreSQL's rows in another order. It is now
flipped where the join is built, and is planned as the LEFT join is. A WHERE
that holds only for a non-null row of the RIGHT join's left side now makes
the join inner, and that side's scan applies it, as it already did for a LEFT
join's right side.

#### Fixed

- `secantus-pgplan` (`joins.rs`): `build_join` flips a RIGHT join into the
  LEFT join of its sides swapped, the ON's equalities and USING columns
  included. The written column order is kept, and a USING column still
  takes the preserved side's value.
- `secantus-pgplan` (`joins.rs`): `preserved_aliases` and
  `equivalence_classes` treat a RIGHT join's left side as a LEFT join's
  right: nullable unless the WHERE proves it non-null.
- Fixes part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_right_join_of_two_tables_is_planned_as_postgresql_plans_it`,
  now a regression test; unit test
  `joins::tests::a_right_joins_left_side_is_nullable_until_the_where_says_otherwise`).
  Measured against PostgreSQL 15.19: 24 of 33 RIGHT-join probes at 20 and
  300 rows return its rows in its order (8 before). None returns other
  rows.

#### Added

- RP-028's open gap is a RIGHT join over a join, whose flipped LEFT join
  has a join as its nullable side, which the join order searches do not
  take:
  `tests/backlog/test_rust_postgres_gaps.py::test_a_right_join_over_a_join_is_searched_as_its_left_join`.
