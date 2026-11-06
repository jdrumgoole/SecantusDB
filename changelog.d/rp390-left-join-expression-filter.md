### Rust PG server: EXPLAIN ANALYZE counts a LEFT Nested Loop's expression Join Filter

EXPLAIN ANALYZE of a LEFT Nested Loop now prints `Rows Removed by Join
Filter` and its children's counts for a Join Filter of any shape
(`select * from s left join e on s.a + 1 < e.a`), not only one comparing
a bare column of each side, as PostgreSQL 15.19 does. The filter is planned
once over both relations and evaluated per pair, as `ExecQual` runs it.

#### Fixed

- A LEFT Nested Loop whose Join Filter was an expression printed no counts
  below it.
  Fixes RP-390, part (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a left nested loop over an expression filter]).
  A Hash Join over filtered scans, a char(n) filter column, Merge Join,
  several hash batches and inner-unique joins stay in RP-390.

#### Found

- RP-395: a LEFT join's ON conjunct over its nullable side alone stays in
  the Join Filter, where PostgreSQL filters that side's scan with it.
