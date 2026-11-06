### Rust PG server: EXPLAIN counts and prices a Hash Join's filtered scans

`EXPLAIN ANALYZE` of a Hash Join whose Seq Scans have their own Filter
(`select * from s join e on s.a = e.a where e.b = 'v1' and s.a > 5`), and
of a parameterized Nested Loop over a filtered outer scan (`from e, k where
e.a < k.a and e.b = 'v1'`), now prints each scan's `actual rows` and `Rows
Removed by Filter` as PostgreSQL 15.19 counts them. A range on a join key
(`s.a > 5`) no longer leaves the Hash Join unpriced: like PostgreSQL, it
restricts its scan and keeps the join clause. Only an equality joins the
key's equivalence class. Before, these joins printed zero costs, and could
hash the wrong side.

#### Fixed

- A Hash Join's outer and hashed Seq Scans, and a parameterized Nested
  Loop's outer Seq Scan, count the rows their Filter rejects. A comma
  join's two-table WHERE conjunct is recognised as its join clause, not
  a scan filter. Part of RP-390, split and fixed as RP-403. Fixes RP-403
  (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a hash join over two filtered scans],
  tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a hash join over a filtered hashed side],
  tests/backlog/test_rp_explain_costs.py::test_plan[hash join over a range on each side's key],
  tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a parameterized nested loop over a filtered scan],
  tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a parameterized nested loop under a join's WHERE]).
- Filed RP-404 (a scan filtered by an expression or a LIKE prints no
  estimate), RP-405 (a join's scan filtered by an expression prints a
  `Filter: (...)` line) and RP-406 (a scan Filter's conjuncts are not in
  `order_qual_clauses`' cost order).
