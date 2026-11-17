### Rust PostgreSQL server: an INTERSECT over a DISTINCT input hashes the smaller input first

PostgreSQL lists first the INTERSECT input it estimates to have fewer
groups, and a group's row comes from that input. A `SELECT DISTINCT`
input was not estimated, so it stayed first even when the other input was
smaller, and the row came from the wrong side. A DISTINCT input's groups
are now its rows, the estimated groups of its select list, as in
PostgreSQL.

#### Fixed

- `select distinct x from t692j intersect select 1.000` returns `1.000`,
  the constant's row, as PostgreSQL 15.19 does, not `1.0`.
  Fixes RP-841, in part (tests/backlog/test_rp_found_rp692.py::test_an_intersect_over_a_distinct_input_puts_the_smaller_input_first)
