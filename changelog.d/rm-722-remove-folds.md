### A `$match` on a constant `$$REMOVE` fails to optimize, as on mongod

A leading `$match` whose `$expr` reads `{$size: "$$REMOVE"}` now fails as
mongod 8.2.11's does, under "Failed to optimize pipeline", before any
document is read: `$$REMOVE` folds as a constant missing.

#### Fixed

- `$$REMOVE` in a `$match`'s `$expr` was not folded, so its fault was an
  executor error.
  Fixes RM-722 (tests/backlog/test_rm_found_2026_11_13_rm524.py::test_a_match_on_a_constant_remove_fails_to_optimize)
