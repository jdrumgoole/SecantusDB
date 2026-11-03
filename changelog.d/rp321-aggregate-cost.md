### Rust PG server: EXPLAIN prices a plain Aggregate of counts

`EXPLAIN select count(*) from t` printed `cost=0.00..0.00 rows=0 width=0`
on its Aggregate. It now prints PostgreSQL 15's `cost_agg` estimate
(AGG_PLAIN) for an Aggregate of `count(*)` / `count(col)` straight over one
relation's Seq Scan, with the scan's own estimate beneath it.

#### Fixed

- A plain Aggregate of counts over a Seq Scan prints PostgreSQL's cost,
  rows and width. Other aggregates, GROUP BY, HAVING and index paths still
  print zeros (their own backlog items).

Fixes RP-321 (tests/backlog/test_rp_explain_costs.py::test_plan[aggregate])
