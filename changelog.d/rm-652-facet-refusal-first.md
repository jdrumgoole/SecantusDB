### A `$facet`'s refusal comes before a later stage's parse error

The Rust MongoDB server now parses a pipeline a stage at a time as mongod
8.2 does: a `$facet` holding a stage it refuses stops the parse there.

#### Fixed

- `[{$facet: {a: [{$documents: ...}]}}, {$project: {}}]` answers 40600, and
  a facet's `$out` before a later `$match`'s `$where` does too, where the
  server answered the later stage's 51272 or `$where` refusal.

Fixes RM-652 (tests/backlog/test_rm_found_2026_11_14_rm530.py::test_a_facet_refusal_comes_before_a_later_stage)
