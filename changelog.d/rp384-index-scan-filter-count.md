### Rust PG server: EXPLAIN ANALYZE prints an index scan's Rows Removed by Filter

An index scan with a Filter now reports, under ANALYZE, how many rows its
Index Cond fetched that the Filter then rejected, as PostgreSQL 15.19's
`show_instrumentation_count` does: the text format prints a non-zero count
after the `Filter` line, and JSON / YAML / XML print `Rows Removed by Index
Recheck` after the `Index Cond` and `Rows Removed by Filter` after the
`Filter`, zero included.

#### Fixed

- `explain (analyze) select * from k where a < 30 and b = 'v5'` printed no
  `Rows Removed by Filter: 26` line.

Fixes RP-384, part (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an index scan's filter]).
