### EXPLAIN estimates a LEFT join read through an index on either side

Some LEFT joins have a WHERE that only the preserved side's index can
answer, as in `k left join e on e.a <> k.a where k.a > 990`. PostgreSQL
reads that side through the index, with the WHERE as its Index Cond. Before
this change the Rust PostgreSQL server printed zeros for these Nested
Loops. It now picks the side's cheapest path and prints PostgreSQL 15.19's
Index Scan, Index Only Scan or Seq Scan, with the join's costs.

The other case is an ON restriction of the preserved side, as in `e left
join k on e.a < k.a and e.a > 990`. Here PostgreSQL reads the nullable side
through its index, parameterized by each outer row. The restriction cannot
move into that scan, so it stays the join's own `Join Filter`. The server
now plans, prices and prints that join the same way.

#### Fixed

- `EXPLAIN` of a Nested Loop Left Join whose WHERE the preserved side's
  index serves gives PostgreSQL's estimate. That side is an Index Scan
  (or Index Only Scan) with the WHERE as its `Index Cond`.
- `EXPLAIN` of a Nested Loop Left Join with a parameterized inner index
  scan and an ON restriction of the outer side gives PostgreSQL's
  estimate. The restriction stays the `Join Filter` and is checked on
  every pair the scan returns. The scan's rows count only the join clauses
  it takes.
- Fixes RP-354 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with a where on an indexed outer],
  tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with a one-sided ON conjunct into an index],
  plus the index-only, two-index, wide-WHERE, two-restriction and literal
  variants beside them). The item stays open for an inner clause over a
  cast and a one-cent rounding tie it now names, along with the other
  nested loops it lists.
