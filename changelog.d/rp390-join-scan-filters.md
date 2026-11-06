### Rust PG server: EXPLAIN ANALYZE counts a join's scan Filters

`EXPLAIN ANALYZE` of a Nested Loop whose Seq Scans carry their own
Filter (`select * from s join e on s.a < e.a where e.b = 'v1'`) now
prints each scan's `actual rows` and `Rows Removed by Filter`, and the
join's `Rows Removed by Join Filter` over the rows those scans kept, as
PostgreSQL 15.19 counts them. Before, the whole join below the top node
went unannotated.

#### Fixed

- A plain Nested Loop's outer Seq Scan and its Materialized (or
  rescanned) inner Seq Scan count the rows their Filter rejects.
  Part of RP-390, split and fixed (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a nested loop over a filtered scan],
  tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a nested loop over two filtered scans]).
- Filed RP-391: an inner join's one-table ON conjunct is left in the Join
  Filter instead of pushed into its scan.
