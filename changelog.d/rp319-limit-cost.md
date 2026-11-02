### Rust PG server: EXPLAIN prices a Limit over a Seq Scan

`EXPLAIN` of a `LIMIT` / `OFFSET` straight over one relation's sequential
scan now prints PostgreSQL 15.19's estimate for both nodes, where both
printed `cost=0.00..0.00 rows=0 width=0` before.

#### Fixed

- A Limit is priced as `adjust_limit_rows_costs` prices it (`pathnode.c`):
  the offset's share of the input moves the startup cost, the count's share
  sets the total, and a constant `LIMIT 0` is estimated as one row. The
  Seq Scan beneath it prints its own estimate. A Limit over a table with an
  index and a filter still prints zeros, since PostgreSQL may keep a cheaper-
  to-start index scan there; so does one above a Sort, Unique or other node.

Fixes RP-319 (tests/backlog/test_rp_explain_costs.py::test_plan[limit])
