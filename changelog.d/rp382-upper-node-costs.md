### Rust PG server: EXPLAIN prices a Sort, Limit or count over an indexed table's scan

A Sort, a Limit or a `count(*)` Aggregate over a table with an index used
to print zero costs when a WHERE reached the index. Each one now prints
PostgreSQL 15.19's plan and estimate over the path PostgreSQL keeps for it.

#### Fixed

- A Sort over an indexed table with a WHERE is priced over the cheapest
  path (a Bitmap Heap Scan, an Index Scan or the Seq Scan), where
  `create_ordered_paths` keeps that Sort over every index scan already in
  the ORDER BY's order.
- A Limit's scan is `get_cheapest_fractional_path`'s: a LIMIT that reads
  a small share of the rows keeps the Seq Scan, which starts at no cost.
- A `count(*)` over an index-only or bitmap scan prints its Aggregate's
  estimate and the scan it reads.

Fixes RP-382 (tests/backlog/test_rp_explain_costs.py::test_plan[a sort over a user table's bitmap scan], tests/backlog/test_rp_explain_costs.py::test_plan[a limit over a user table's index scan])
