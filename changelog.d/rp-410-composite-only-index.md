### A table whose only index is composite is priced through it

A WHERE that bounds both columns of a two-column index, on a table with no
other index, is now planned and priced as PostgreSQL 15.19 plans it: `select
* from t2 where a = 15 and b = 3` over `t2_ab (a, b)` is `Index Scan using
t2_ab on t2 (cost=0.28..8.30 rows=1 width=11)` with both equalities in its
`Index Cond`, where it used to print an unpriced Seq Scan.

#### Fixed

- The Rust PostgreSQL server's scan estimate no longer gives up on a qual
  over a column an index holds only past its lead when the same WHERE
  bounds that index's leading column: it is that index's qual, as
  `match_clause_to_indexcol` makes it.
  Fixes RP-410 (tests/backlog/test_rp_explain_costs.py::test_plan[a table's only index composite])
