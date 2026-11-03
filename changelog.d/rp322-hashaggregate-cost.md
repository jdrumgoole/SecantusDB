### Rust PG server: EXPLAIN prices a HashAggregate of grouped counts

`EXPLAIN select b, count(*) from e group by b` printed `cost=0.00..0.00
rows=0 width=0` on its HashAggregate. It now prints PostgreSQL 15's
`cost_agg` estimate (AGG_HASHED, `estimate_num_groups` for the rows) over
the Seq Scan's own estimate.

#### Fixed

- A GROUP BY of plain columns with `count(*)` / `count(col)` straight over
  one table's Seq Scan prints PostgreSQL's cost, rows and width on the
  HashAggregate, where PostgreSQL keeps the hashed plan. Other aggregates,
  HAVING, ORDER BY, expression keys, tables with indexes and plans whose
  hash table would spill still print zeros.

Fixes RP-322 (tests/backlog/test_rp_explain_costs.py::test_plan[hashaggregate])
