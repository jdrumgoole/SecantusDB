### `$planCacheStats` sizes entries over an object-form `$elemMatch`

A plan cache entry whose query holds `{d: {$elemMatch: {e: 3}}}` now
reports `estimatedSizeBytes` as mongod 8.2.11 does, whether the winning
plan reads the `$elemMatch` from its index or not.

#### Fixed

- The entry's `PlanCacheIndexTree` holds an `ELEM_MATCH_OBJECT` over its
  predicate, both tagged when the winner reads `d.e_1`: 3230 and 3656 bytes
  on macOS arm64, mongod's figures.

Fixes RM-694 in part (tests/backlog/test_rm_found_2026_11_08_rm341.py::test_an_object_elem_match_entry_reports_its_size)
