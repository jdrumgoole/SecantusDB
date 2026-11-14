### Rust MongoDB server: an `$in` and a simple projection explain as SBE under `queryFramework: "sbe"`

A `find` that a query setting puts in SBE now explains as the slot program
mongod 8.2.11 runs when its filter holds an `$in` or it projects top-level
fields. Before, it was explained in the classic shape.

#### Fixed

- An `$in` is `isMember` against an `InList` that the plan's environment
  binds, holding the values in the command's order. A simple projection is
  `makeBsonObj` (closed for an inclusion, open for an exclusion), or
  `newBsonObj` when it keeps one field. It sits above a skip and below a
  limit. The `queryPlan`, the slot program and `executionStats` match
  mongod's. A sorted or index-served `find` is still explained classically
  (RM-543 stays open for those).

Fixes RM-543 in part (tests/backlog/test_rm_found_2026_11_10_rm418.py::test_sbe_find_in_and_projection_are_mongods).
