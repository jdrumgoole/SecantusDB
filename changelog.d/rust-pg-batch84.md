### The Rust PostgreSQL server: joins on several equalities are planned as PostgreSQL plans them

`a JOIN b ON a.x = b.x AND a.y = b.y` now returns its rows in the order
PostgreSQL 15.19 does: a Merge Join with both sides sorted on every key, or
a Hash Join on all of them. Before, a join on more than one equality was not
priced, and its rows came out in the general path's order.

#### Fixed

- `secantus-pgplan`: `JoinSide` carries up to three further join keys
  (`ExtraKeys`). A join's selectivity multiplies each clause's, each `=` is
  costed, and a hash join's bucket size is the smallest of its clauses'
  (`final_cost_hashjoin`).
- `secantus-pg`: a join of two tables on two to four column equalities,
  INNER (two or more equivalence classes spanning both sides) or LEFT, is
  priced and run.
  - A merge join sorts each side on every key in clause order.
  - A hashed side kept in its table's order hashes the keys combined as
    `ExecHashGetHashValue` does: rotated left a bit before each key's hash
    is XORed in.

  54 of 54 shapes match 15.19 at 20 to 3,000 rows, with a primary key and
  with NULL keys (14 before). Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_join_on_two_equalities_is_planned`).

RP-028 stays open. A three-table join whose WHERE reads two of the tables is
its new gap test
(`tests/backlog/test_rust_postgres_gaps.py::test_a_where_over_two_of_three_tables_is_planned`).
