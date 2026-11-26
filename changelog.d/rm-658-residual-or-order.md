### A FETCH's residual `$or` lists its branches in mongod's order

`explain` on the Rust MongoDB server now sorts the children of a logical
node a FETCH rechecks the way mongod 8.2's `sortUsingTags` does, by path and
then match type, at every level rather than only the top.

#### Fixed

- `{k: {$gte: 2}, $or: [{"x.a": 1}, {x: {$type: [4]}}, {x: {$not: {$type: [3]}}}]}`
  behind an index on `k` lists the FETCH's `$or` as `$not`, `$type`, `x.a`,
  in a `find` and in an aggregate's `$replaceRoot` rewrite.

Fixes RM-658 (tests/backlog/test_rm_found_2026_11_14_rm536.py::test_a_residual_or_lists_its_branches_as_mongod_does)
