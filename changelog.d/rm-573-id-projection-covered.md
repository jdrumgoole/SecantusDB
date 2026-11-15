### Rust MongoDB server: an `{_id: 1}` projection is answered from an index

A projection of `_id` alone is now covered by an index that holds `_id`, as
on mongod 8.2.11: `find({y: 0}, {_id: 1})` over `{y: 1, _id: 1}` plans
`PROJECTION_COVERED <- IXSCAN` instead of fetching each document.

#### Fixed

- `covers` no longer refuses an inclusion projection of `_id` alone; `{_id: 0}` alone stays an exclusion.
  Fixes RM-573 (tests/backlog/test_rm_found_rm471.py::test_an_id_projection_is_covered)
