### `$planCacheStats` sizes an `$or` entry with a branch of several predicates

The Rust MongoDB server now reports `estimatedSizeBytes` for a plan-cache
entry whose `$or` has a branch of several predicates, building that
branch's `AND` in the `PlanCacheIndexTree` as mongod 8.2.11 does.

#### Fixed

- `{$or: [{a: 3, c: 2}, {b: 5}], c: {$lt: 3}}` over `a_1`, `b_1` and `c_1`
  sizes at mongod's 10381 (macOS arm64) instead of leaving the field out.

Fixes RM-694 in part (tests/backlog/test_rm_found_2026_11_08_rm341.py::test_an_or_entry_with_a_compound_branch_reports_its_size)
