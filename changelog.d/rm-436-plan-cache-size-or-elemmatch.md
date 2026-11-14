### Rust MongoDB server: `$planCacheStats` sizes `$or` and `$elemMatch` entries

A plan cache entry whose winner reads an `OR` of index scans beside other
clauses, or whose query holds an `$elemMatch` its index does not read, now
reports mongod 8.2.11's `estimatedSizeBytes` on macOS arm64 instead of
leaving the field out.

#### Fixed

- The entry's `PlanCacheIndexTree` is built for an `OR` over single-predicate
  branches (each tagged with the index it reads, `tagForSort` taking the
  lowest-numbered) and for an `ELEM_MATCH_VALUE` node, and an `OR` stage's
  `OrStats` is sized.

Fixes RM-436 (tests/backlog/test_rm_found_2026_11_08_rm341.py::test_entries_this_does_not_size_report_their_size)
