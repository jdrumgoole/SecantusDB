### Rust PG server: EXPLAIN prices a nested loop into an index on an inequality

`EXPLAIN` of a join whose only clauses are inequalities between the two
tables, over a table with a B-tree on the compared column, now prints the
plan PostgreSQL 15.19 picks: a Nested Loop whose inner side is an Index Scan
parameterized by the outer row, with `cost_index`'s estimate over the outer
side's rows as its loop count, where it printed a Nested Loop of two Seq
Scans with zeros.

#### Fixed

- The parameterized Index Scan is weighed against the plain and
  materialised nested loops, for an inner join (either side inner) and for a
  LEFT join's nullable side. Its `Index Cond` is the clause written inner
  column first (`Index Cond: (a > e.a)` for `e.a < k.a`); the join's other
  clauses move into the scan as its `Filter` and cut its rows, and a `<>`
  the `Index Cond` already implies is costed but not printed, as
  `create_indexscan_plan` drops it.
- Still zeros, and still in RP-354: the same scan as an Index Only Scan, two
  bounds on the index column, a clause over an expression of the inner side,
  and a key on a later index column.

Fixes RP-354, part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop into an index],
[nested loop into an index with a moved filter], [nested loop left join into an index],
[nested loop into an index with an implied filter]).
