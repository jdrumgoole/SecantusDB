### Rust PG server: EXPLAIN prices a UNION ALL's Append

`EXPLAIN select a from e union all select a from k` printed
`cost=0.00..0.00 rows=0 width=0` on the Append and on both of its Seq
Scans. It now prints PostgreSQL 15's estimates: each side's sequential
scan, and over them `cost_append`, which starts with the first child,
totals the children plus half a `cpu_tuple_cost` per row, and has as its
width the children's widths weighted by their rows. A side with a WHERE
stays a subquery in PostgreSQL, so the Append is priced over its Subquery
Scan (one more `cpu_tuple_cost` per row), as PostgreSQL does.

#### Fixed

- A `UNION ALL` of two plain selects of user tables prints PostgreSQL's
  cost, rows and width on its Append and its scans.

Fixes RP-324 (tests/backlog/test_rp_explain_costs.py::test_plan[append])
