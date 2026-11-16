### EXPLAIN draws an IN or EXISTS subquery as PostgreSQL's semi join

PostgreSQL pulls a WHERE's `IN (SELECT ...)` or correlated `EXISTS` up into
a join, so `EXPLAIN` of `select * from e where a in (select a from s)` shows a
`Hash Semi Join` of the two scans, with the subquery's table hashed. The Rust
PostgreSQL server now draws that plan for a WHERE that is one such sublink,
instead of a single scan with an opaque `Filter: (...)`. Its costs are not yet
modelled, and an inner side unique on its column, which PostgreSQL turns into
an inner join, still prints the old shape.

#### Fixed

- Rust PG server: EXPLAIN of a WHERE that is one uncorrelated `IN` / `= ANY`
  subquery, or one `EXISTS` correlated by a column equality, prints `Hash Semi
  Join` with its `Hash Cond`, as PostgreSQL 15.19 does.
  Partly fixes RP-411 (tests/backlog/test_rp_explain_costs.py::test_plan[an in subquery is a hash semi join], tests/backlog/test_rp_explain_costs.py::test_plan[an exists subquery is a hash semi join]).
