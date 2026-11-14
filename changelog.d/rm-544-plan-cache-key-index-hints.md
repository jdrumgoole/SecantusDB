### Rust MongoDB server: a query setting's `indexHints` moves the `planCacheKey`

A read under query settings that carry `indexHints` now reports mongod
8.2.11's `planCacheKey`: the hints are hashed as `query_settings_hash.cpp`
hashes them (boost 1.88's string hash for index names and namespaces,
`SimpleBSONObjComparator`'s hash for key patterns, the direction for a
`$natural` hint), where they were hashed as unset.

#### Fixed

- `planCacheKey` under a setting's `indexHints` matches mongod's.
  Fixes RM-544 (tests/backlog/test_rm_found_2026_11_10_rm418.py::test_plan_cache_key_hashes_index_hints)
