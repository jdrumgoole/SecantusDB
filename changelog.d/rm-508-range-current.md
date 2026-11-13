### Rust MongoDB server: a `range` window's `"current"` is the document answered

A `$setWindowFields` `range` window bounded by `"current"` now ends (or
starts) at the document answered, as mongod 8.2's does, rather than at its
sort value, which took in every document tied with it.

#### Fixed

- Over `n` of 1, 1, 2, 2, 2, 3, a `$push` of `_id` over `range:
  ["unbounded", "current"]` gives `[0], [0, 1], [0, 1, 2], ...` as mongod
  does, not `[0, 1], [0, 1], [0, 1, 2, 3, 4], ...`; the same for a
  `"current"` lower bound, and for the removable accumulators (`$sum`,
  `$min`, `$count`) that move with the window.
  Fixes RM-508 (tests/backlog/test_rm_found_2026_11_10_rm404.py::test_a_range_windows_current_is_the_document;
  regression test `secantus-core` `range_current_tests::a_range_windows_current_is_the_document`).
