### Rust PG server: EXPLAIN prices a single relation's Seq Scan

EXPLAIN no longer prints every cost as zero. Joe withdrew the scope
decision that kept them there (2026-11-02), and a plan that is one
relation's sequential scan now prints PostgreSQL 15.19's estimate: the
startup and total cost (`cost_seqscan`), the rows (`clamp_row_est` of the
tuples through the WHERE's selectivity) and the width (each output column's
`stawidth`, or its type's average). A user table is sized as the planner's
cost model already sized it; a built-in catalog from what `initdb` left --
its `relpages` / `reltuples`, its heap's blocks and its `pg_statistic` rows,
or a database-wide ANALYZE's where that re-measured a column.

#### Fixed

- `explain select * from pg_class where relkind = 'v'` is `Seq Scan on
  pg_class  (cost=0.00..19.12 rows=140 width=265)`, as on PostgreSQL in a
  fresh database, and a user table's scan is priced the same way, analysed
  or not. Fixes RP-314
  (tests/backlog/test_rp_catalog_reads.py::test_a_catalog_scan_is_estimated,
  tests/backlog/test_rp_explain_costs.py::test_plan).
- `pg_class` shows the built-in relations' `relpages`, `reltuples` and
  `relallvisible` as `initdb` left them (`pg_class` itself: 14, 410, 14)
  rather than 0, -1 and 0.
- `EXPLAIN (FORMAT JSON)` (and YAML, XML) lists a node's costs before its
  `Filter`, in PostgreSQL's order.

#### Added

- Each other plan node kind's estimate is filed as its own item with a gap
  test (RP-317 to RP-333), with two found along the way: a catalog read by
  its index is a Seq Scan here (RP-334), and a `name` column compared with
  a literal is shown cast to `text` (RP-335).
