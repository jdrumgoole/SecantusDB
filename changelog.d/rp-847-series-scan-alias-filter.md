### EXPLAIN names a series scan's alias and prints its Filter

EXPLAIN of a SELECT over `generate_series` in FROM now reads as
PostgreSQL's. The Function Scan names the alias it was given
(`Function Scan on generate_series f`), and the WHERE the scan applies to
each row prints as its `Filter:` line, deparsed as ruleutils does
(`Filter: ((x + 1) > 0)`). Before, every such scan printed as a bare
`Function Scan on generate_series`.

#### Fixed

- Rust PG server: a `generate_series` Function Scan prints its alias and
  its `Filter`. The estimate of a filtered series scan stays open under
  RP-847.
  Fixes RP-847, part (tests/backlog/test_rp_found_rp703.py::test_a_series_scan_explains_its_alias_and_filter)
