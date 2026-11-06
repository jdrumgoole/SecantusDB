### Rust PG server: EXPLAIN ANALYZE counts a LEFT Nested Loop's Join Filter

EXPLAIN ANALYZE of a LEFT Nested Loop whose Join Filter compares a column
of each side (`select * from s left join e on s.a < e.a`) now prints its
`Rows Removed by Join Filter` and its children's actual rows and loops, as
PostgreSQL 15.19 does. The filter is run over every pair, because a LEFT
join's null-extended rows hide how many pairs it rejected.

#### Fixed

- A LEFT Nested Loop with a Join Filter printed no counts below it.
  Fixes RP-389 (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a left nested loop's join filter]).
  What it leaves (filtered scans, Merge Join, several hash batches,
  inner-unique joins) is RP-390.
