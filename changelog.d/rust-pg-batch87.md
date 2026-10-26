### The Rust PostgreSQL server: a WHERE equality between joined tables is a join clause

`a JOIN b ON ... JOIN c ON ... WHERE a.x = c.y` now returns its rows in the
order PostgreSQL 15.19 does. PostgreSQL takes the WHERE's equality as a join
clause between `a` and `c`, so the tables' clauses can form a cycle. Two
relations are then joined on every clause between them, for example a Merge
Join on `(b.y = c.a) AND (a.x = c.y)` with both sides sorted on both keys.
Before, the join order search turned off for any WHERE equality between
tables.

#### Fixed

- `secantus-pgplan`: a join source carries its WHERE's equalities between
  two tables' columns (`JoinRows::where_equalities`).
- `secantus-pg`: the join order search takes them as edges (inner clauses
  of no ON), and three tables whose clauses make a cycle go through it. An
  inner join's further key pairs, from a comma join's WHERE or an ON of
  several equalities, are edges too, and an edge already present is not
  added twice.
- `secantus-pg`: two relations are joined on every edge between them. The
  first edge is the key and the rest are further keys, priced with the
  multi-key costs, sorted on and matched on. An equivalence class's clause
  leads where one is among them, as in PostgreSQL's merge condition, and
  that join's output stays in the class's order for a merge above. A hash
  right or full join, a merge fill and a batched hash join on several keys
  are declined. 14 of 14 cyclic shapes of three and four tables match
  15.19 at 20 and 300 rows (none before). Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_where_equality_over_two_of_three_tables_is_planned`).

RP-028 stays open. A LIMIT over three tables, planned for its first rows,
is its new gap test
(`tests/backlog/test_rust_postgres_gaps.py::test_a_limit_over_three_tables_is_planned`).
