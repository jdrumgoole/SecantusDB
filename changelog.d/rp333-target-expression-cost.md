### Rust PG server: EXPLAIN prices a Seq Scan whose select list computes

`EXPLAIN` of one relation's sequential scan whose select list holds an
operator expression or a constant (`select a + 1 from e`, `select 1 from
e`) now prints PostgreSQL 15.19's estimate, where it printed
`cost=0.00..0.00 rows=0 width=0` before.

#### Fixed

- The select list is costed as `cost_seqscan` costs its target: each
  operator over a column is one `cpu_operator_cost` per output row, a
  constant or a subtree reading no column is folded and free, and a computed
  entry's width is its type's average width. A cast, a function call, an
  operator outside the built-in arithmetic, comparison and `||` set, or an
  index path chosen for the scan still prints zeros.

Fixes RP-333 (tests/backlog/test_rp_explain_costs.py::test_plan[a seq scan of an expression])
