### The Rust MongoDB server keeps its indexes in the order they were built

mongod lists a collection's indexes in the order they were built, checks
them for a duplicate key in that order, and tries their plans in it. The
Rust MongoDB server kept no build order and used name order for all three.
So `listIndexes` after `create_index("z")` then `create_index("a")`
answered `a_1, z_1`, and a document duplicating both unique indexes was
refused by `a_1` where mongod names `z_1`.

#### Fixed

- **Each index records its build order** in a catalog option, `buildSeq`,
  stripped from `listIndexes` like the other catalog bookkeeping.
  `listIndexes`, the duplicate-key check and the planner's catalog all
  follow it, with `_id_` first. A recreated index goes to the end, as on
  mongod. A store written before this keeps its indexes in name order,
  ahead of any built later.

Fixes RM-081, which also covered RM-107
(tests/backlog/test_rm_057_084.py::test_list_indexes_in_build_order,
::test_duplicate_key_names_the_first_built_unique_index,
tests/backlog/test_rm_085_112.py::test_list_indexes_in_creation_order).
