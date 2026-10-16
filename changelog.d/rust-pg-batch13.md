### Rust PostgreSQL server: backlog batch 13

More of the Rust PostgreSQL server's backlog, measured against PostgreSQL
15.19.

#### Fixed

- **A join whose equalities chain returns its rows in PostgreSQL's
  order.** PostgreSQL gathers the equalities of a join's ON and WHERE
  into equivalence classes (`generate_base_implied_equalities`): with
  `on l.b = r.c where r.c = l.a`, `l.a = l.b` filters the left side and
  `l.b = r.c` is the one join clause; with `where l.b = 2`, both sides
  are filtered by the constant and an inner join has no join clause
  left, so it is a nested loop over every pair (a LEFT join's inner side
  takes the constant too, `reconsider_outer_join_clauses`). The server
  now derives the same classes for a join of two tables, filters each
  side by what they imply before putting it in order, and prices the
  plan as PostgreSQL does, including a column equality's default
  selectivity. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_join_with_a_second_equality_between_its_sides).
- **A join of three tables returns its rows in the order of PostgreSQL's
  plan.** For `x JOIN y ON .. JOIN z ON ..`, PostgreSQL searches the
  join orders (`standard_join_search`): each pair of tables a clause
  connects, priced every way, then each such pair with the third table,
  every candidate in one `add_path` list, in the orders the outer joins
  allow. A LEFT chain often joins its last two tables first, as a hash
  right join, sorts that and merges it with the first. The server now
  makes the same search for three stored tables on two column
  equalities, inner or LEFT, and builds the rows in the plan it picks,
  including a hash right join's rows nothing matched in its hash table's
  order (`ExecScanHashTableForUnmatched`, by the type's own hash
  function); a two-table LEFT join planned as a hash right join is
  ordered so too. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_three_table_left_join_chain_merges_in_key_order).
- **A WHERE over an ANALYZEd table is estimated from its statistics.**
  ANALYZE now also keeps each column's most common values, histogram and
  correlation as `compute_scalar_stats` builds them, and a WHERE of
  comparisons with constants gets PostgreSQL's selectivities from them
  (`var_eq_const`, `scalarineqsel` with the histogram's ends read from an
  index, `clauselist_selectivity`'s range pairs). An index scan's cost now
  reads the column's correlation, and a Group over a correlated index scan
  is weighed as PostgreSQL weighs it, so `select g from t where k > 5
  group by g` and `select k from t where k < 50 group by k` over analysed
  tables come out as 15.19 returns them. 120 of 120 such shapes match at
  20 to 10,000 rows, with and without rows inserted after the ANALYZE. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_where_over_an_analyzed_table_estimates_from_its_histogram).
