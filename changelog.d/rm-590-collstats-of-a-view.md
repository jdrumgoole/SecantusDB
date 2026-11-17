### Rust MongoDB server: a `$collStats` of a view answers, as on mongod

A `$collStats: {}` or `{latencyStats: {}}` stage over a view now answers
one document naming the view, as mongod 8.2.11 does, instead of refusing
it; `count`, `storageStats` and `queryExecStats` are still refused (166).

#### Fixed

- A `$collStats` of a view is no longer resolved to the collection beneath
  it, and is refused only for the statistics a view does not keep.
  Fixes RM-590 (tests/backlog/test_rm_view_namespaces.py::test_a_coll_stats_stage_of_a_view_answers)
