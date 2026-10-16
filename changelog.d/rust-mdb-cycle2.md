### Rust MongoDB server: validator collation, `2dsphere` levels, and a refused index spec

Differences from mongod 8.2.11, each pinned by a test that compares the
two.

#### Fixed

- **A collection validator compares under the collection's collation.**
  An update that named a collation of its own (`simple`, `fr`) had its
  validator checked under that one. With a collection collation of `en`
  strength 2, `{k: "A"}` refused `k: "a"` with 121 where mongod accepts
  it.
- **`coarsestIndexedLevel` / `finestIndexedLevel` are accepted on a
  `2dsphere` index**, listed after `2dsphereIndexVersion`, and checked as
  mongod checks them: 16747, 16748, 16749 (the default coarsest level is
  2), 14 for a non-number and 2 for a fraction. They were refused (197).
- **A `createIndexes` refused for a spec with no `name` creates
  nothing.** The collection it would have created was left behind.

Fixes RM-096 (tests/backlog/test_rm_085_112.py::test_validator_reads_the_collections_collation),
RM-149 (tests/backlog/test_rm_geo_residuals.py::test_2dsphere_cell_levels_accepted_and_listed,
::test_2dsphere_cell_levels_are_checked) and RM-150
(::test_refused_spec_leaves_no_created_collection).
