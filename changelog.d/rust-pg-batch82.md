### The Rust PostgreSQL server: a join's ON reduces the outer joins below it

`a FULL JOIN b ON ... JOIN c ON b.x = c.y` now returns its rows in the order
PostgreSQL 15.19 does. The inner join's ON can never pass a row where `b` is
NULL, so PostgreSQL plans the FULL join below it as a LEFT join, and so does
this server now.

#### Fixed

- `secantus-pgplan`: `reduce_outer_joins` takes what each join's ON proves
  non-NULL, not only the WHERE's. An inner join passes the tables its
  strict ON reads to both its sides, a LEFT join to its nullable side and a
  RIGHT join to its own, and a FULL, LEFT or RIGHT join below whose
  nullable side holds one is made LEFT or inner. The reduction is decided
  before the join's sides are planned, as `reduce_outer_joins_pass2` does.
  22 of 24 three- and four-table shapes match 15.19 at 20 and 300 rows (10
  before). Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_full_join_under_an_inner_join_is_searched`).

RP-028 stays open. A FULL join that nothing reduces, inside three or more
tables, is its new gap test
(`tests/backlog/test_rust_postgres_gaps.py::test_a_full_join_below_a_left_join_is_searched`).
