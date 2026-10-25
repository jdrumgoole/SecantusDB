### The Rust PostgreSQL server: FULL joins in every join the search takes, and outer joins reduced by an ON

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

- `secantus-pg`: a FULL join that stays FULL in a chain of three tables
  goes through the join order search. A FULL join's minimum sides are its
  whole ones (`make_outerjoininfo`), so its pair joins first. Each FULL
  step is priced and run as a two-table FULL join is: a Merge or Hash Full
  Join, either side outer, with the rows nothing matched where PostgreSQL
  puts them. 48 of 48 shapes match 15.19 at 20 to 3,000 rows, with and
  without NULL keys (6 before). Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_full_join_below_a_left_join_is_searched`).

- `secantus-pg`: a FULL join in four to eight tables goes through the join
  order search. Its `SpecialJoinInfo` keeps both its sides whole, a join
  over it that overlaps either side takes both, and `join_is_legal` never
  commutes it. 36 of 36 shapes of four and five tables match 15.19 at 20
  and 300 rows, with and without NULL keys (4 before). A hash full join
  split into batches is declined, since its unmatched rows come out batch
  by batch, which is not modelled. Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_full_join_in_four_tables_is_searched`).
- `secantus-pgplan`: a WHERE that makes one side of a FULL join non-null is
  pushed to that side's scan, which the reduction has made a preserved
  side, so its rows are filtered before they are sorted, as in
  PostgreSQL.
- `secantus-pg`: a FULL join whose ON holds more than its equality is
  planned as a Hash Full Join only, since `select_mergejoin_clauses`
  allows no merge full join with a join filter. The rows the filter
  rejects come out where PostgreSQL's hash full join puts them. 20 of 20
  two-table variants match: WHERE, LIMIT, ORDER BY, USING, and an ON
  filter.

RP-028 stays open. A LEFT join over an inner join's equivalence class is
its new gap test
(`tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_over_an_inner_class_is_searched`).
