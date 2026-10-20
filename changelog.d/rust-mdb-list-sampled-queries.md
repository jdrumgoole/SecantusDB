### Rust MongoDB server: `$listSampledQueries`

#### Added

- `$listSampledQueries` runs, and lists no queries. This server has no
  query analyzer (`configureQueryAnalyzer` is 59), so none have been
  sampled; mongod 8.2.11 gives the same answer on a set that never
  configured one. The stage's argument and placement are checked as on
  mongod (6876000, 14, 73). It used to be 40324, an unknown stage.

RM-215 part (`tests/backlog/test_rm_found_2026_10_20b.py::test_list_sampled_queries_as_mongod`).
`$planCacheStats` and `$queryStats` remain open.
