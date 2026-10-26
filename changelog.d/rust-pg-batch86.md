### The Rust PostgreSQL server: a WHERE over two of three joined tables filters the join PostgreSQL filters

`a JOIN b ON ... JOIN c ON ... WHERE a.x > b.y AND b.y < c.z` now returns its
rows in the order PostgreSQL 15.19 does. PostgreSQL applies each multi-table
WHERE conjunct as a join filter of the first join that holds its tables.
That filters the rows before the join above sorts or hashes them, and
changes the join's estimated size.

#### Fixed

- `secantus-pgplan`: a join source carries its WHERE's multi-table
  conjuncts as join filters (`JoinRows::where_filters`). Each records its
  tables, its row expression, its operator count, and how PostgreSQL
  estimates it with no statistics: `DEFAULT_INEQ_SEL` for `<`, `<=`, `>`
  and `>=` (between columns or expressions), `neqjoinsel` for `<>` and IS
  DISTINCT FROM between columns, `var_eq_const` for a column equal to a
  constant, and an OR arm by arm. `JoinRel::set_extra_cost` prices a join
  filter's operators on every row its join makes (`qp_qual_cost`).
- `secantus-pg`: the three-table join search places each join filter on
  the pair joined first that holds its tables, with its selectivity in the
  pair's rows, or else on the top join, and applies a pair's filters to
  its rows as the pair is joined. 117 of 117 shapes match 15.19 at 20 to
  2,000 rows (9 before). Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_where_over_a_pair_joined_first_is_planned`).

RP-028 stays open. A WHERE equality over two of three tables, which joins an
equivalence class of its own, is its new gap test
(`tests/backlog/test_rust_postgres_gaps.py::test_a_where_equality_over_two_of_three_tables_is_planned`).
