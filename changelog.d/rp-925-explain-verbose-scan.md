### EXPLAIN VERBOSE prints a scan's output list and qualified names

`EXPLAIN VERBOSE` of a query that is one table's scan now prints what
PostgreSQL prints: the relation schema-qualified (`Seq Scan on public.k`),
the scan's `Output` list, and its `Filter` or `Index Cond` with each column
prefixed by its relation (`(k.a > 5)`). Before, VERBOSE was accepted and
then ignored. Plans with nodes above the scan still print as without
VERBOSE (RP-966).

#### Fixed

- Rust PG server: `EXPLAIN VERBOSE` of a single-relation scan prints the
  schema-qualified relation, the `Output` list and prefixed quals.
  Fixes RP-925 (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose prints a scan's output list])
