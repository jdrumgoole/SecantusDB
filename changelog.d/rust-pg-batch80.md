### The Rust PostgreSQL server: FULL joins and nested LEFT joins are planned as PostgreSQL plans them

A FULL join of two tables now returns its rows in the order PostgreSQL
15.19 does. So does a query such as `a LEFT JOIN (b LEFT JOIN c ON ...) ON
...`. Before, both came out in the general join path's order: the same
rows, in a different order.

#### Fixed

- `secantus-pg`: a FULL join of two stored tables on one column equality
  is priced and run as a Merge Full Join or a Hash Full Join, either side
  outer. A hash full join returns the hashed rows nothing matched last, in
  its hash table's order. A merge full join puts them in their key's
  place, and a row with a NULL key where its scan reaches it. 56 of 56
  shapes match 15.19 at 20 to 3,000 rows a side, with and without a
  primary key and NULL keys; none did before. Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_full_join_is_planned`).
- `secantus-pgplan`: a WHERE that no NULL-extended row of a FULL join's
  side passes makes it a LEFT, RIGHT or inner join, as
  `reduce_outer_joins` does.
- `secantus-pg`: the join order search takes a LEFT join whose nullable
  side holds another LEFT join. Each outer join's minimum sides are widened
  by a lower outer join's, as `make_outerjoininfo` widens them. 18 of 20
  nested shapes of three and four tables match 15.19 at 20 and 300 rows.
  The other two mix an equivalence class with a LEFT join, which RP-028
  still lists. Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_whose_nullable_side_holds_a_left_join_is_searched`).
- `secantus-pg`: a LEFT join's ON clause makes the tables it reads non-null
  inside its nullable side, so a LEFT join there becomes inner, as in
  `reduce_outer_joins`.
- `secantus-pg`: an ALTER TABLE that builds an index (ADD PRIMARY KEY, ADD
  UNIQUE, or a rewrite of an indexed table) records the table's size, as
  CREATE INDEX does (`index_update_stats`).

RP-028 stays open. A FULL join inside a join of three or more tables is
its new gap test
(`tests/backlog/test_rust_postgres_gaps.py::test_a_full_join_under_an_inner_join_is_searched`).
