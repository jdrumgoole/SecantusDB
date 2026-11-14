### EXPLAIN prices a filtered generate_series scan

An `EXPLAIN` of a `generate_series` in FROM with a WHERE now carries
PostgreSQL's estimate instead of `rows=0 width=0`. As `cost_functionscan`
does, every row the series yields pays the tuple cost and the Filter's
operators, and the row estimate is the series' rows times the WHERE's
selectivity over a relation with no statistics: an equality is one over
the series' rows (up to 200 distinct), an inequality a third, a `<` and
`>` pair on the column 0.005, and an OR combines its arms.

#### Fixed

- `explain select 1 from generate_series(1, 2) f(x) where x > 1` prints
  `(cost=0.00..0.03 rows=1 width=4)`, as PostgreSQL 15.19 does.
  Fixes RP-847 (tests/backlog/test_rp_found_rp703.py::test_a_filtered_series_scan_is_priced)
