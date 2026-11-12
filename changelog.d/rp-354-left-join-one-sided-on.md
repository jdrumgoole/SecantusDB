### EXPLAIN estimates a LEFT join's ON conjunct over its preserved side

A LEFT join's ON can hold a condition on its preserved side alone, such as
`e left join e e2 on e.a <> e2.a and e.a > 990`. PostgreSQL cannot push
that condition into the scan, because a row that fails it still comes out,
NULL-extended. So it stays in the join's filter, and the planner estimates
it from the table's column statistics. The Rust PostgreSQL server's EXPLAIN
now does the same: it prints PostgreSQL 15.19's cost and row estimate for
that Nested Loop where it printed zeros before. As in PostgreSQL, a single
bound against a constant is multiplied in with the range clauses, so two
bounds on one column count as one range.

#### Fixed

- `EXPLAIN` of a Nested Loop Left Join whose ON holds a conjunct over the
  preserved side alone gives PostgreSQL's estimate. The conjunct is
  lowered to a filter over its table, and its selectivity comes from that
  table's statistics, as `clause_selectivity` computes it for a restriction
  clause. A shape it does not model still prints zeros: an inner side also
  read through a parameterized index, or a conjunct that costs something
  other than its operators (an IN list, a pattern, a cast of a column).
- A BETWEEN in a join's filter is priced as the two comparisons
  `transformAExprBetween` expands it into, or four when SYMMETRIC. Before,
  it was priced as none.
- Fixes RP-354 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with a one-sided ON conjunct],
  plus the two-bound, equality and primary-key variants beside it). The
  item stays open for a LEFT join with a WHERE, a parameterized inner index
  scan, and the other nested loops it lists.
- Filed RP-902 (a LEFT join reduced to an inner one keeps its ON's
  one-sided conjunct in its Join Filter) and RP-903 (a BETWEEN among a
  join's ON conjuncts prints as one nested AND).
