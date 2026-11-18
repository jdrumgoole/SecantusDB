### Rust MongoDB server: a `$match` reading a missing sort key fails as mongod's does

With no `$sort` ahead of it, a `$match` whose `$expr` reads `{$meta:
"sortKey"}` now fails with mongod 8.2.11's executor-error envelope and
names the operand `missing`, not `undefined`.

#### Fixed

- `{$meta: ...}` no longer counts as a constant expression, so its errors
  carry the executor envelope; a query filter's sort key is reported as
  missing.

Fixes RM-633 (tests/backlog/test_rm_found_2026_11_13_rm524.py::test_a_match_reads_a_missing_sort_key)
