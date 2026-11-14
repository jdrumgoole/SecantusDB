### Rust MongoDB server: an explain's `queryPlanner` carries `optimizationTimeMillis`

Every explained query on mongod 8.2.11 -- `find`, `aggregate`, `count`,
`distinct`, `update`, `delete`, `findAndModify`, over a missing collection
too -- reports `optimizationTimeMillis` after `planCacheKey`; the Rust
server gave it only for a change stream.

#### Fixed

- `queryPlanner.optimizationTimeMillis` (0) in every explain.
  Fixes RM-545 (tests/backlog/test_rm_found_2026_11_10_rm418.py::test_find_explain_gives_optimization_time)
