### Rust MongoDB server: a FETCH rechecks an index's inexact predicate first

A candidate plan's FETCH filter now lists the predicates tagged with its
index -- an inexact `$exists` or `$regex` -- ahead of the others, as
mongod 8.2.11's `sortUsingTags` does.

#### Fixed

- `{a: {$lt: 5}, b: {$exists: true}}` over `b_1` rechecks `{$and: [{b:
  {$exists: true}}, {a: {$lt: 5}}]}`, not `a` first.
  Fixes RM-439 (tests/backlog/test_rm_found_2026_11_08_rm341.py::test_an_exists_candidates_filter_is_in_mongods_order)
