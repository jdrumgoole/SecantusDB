### A `$match` on `ns` after a change stream joins the oplog read

The Rust MongoDB server now rewrites an `$eq` or `$in` on `ns`, `ns.db` or
`ns.coll` that follows a `$changeStream` into the oplog filter as mongod
8.2.11 does: the commands' `o` fields that name the namespace, and a
write's `ns`. Its `explain` and `queryHash` match mongod's for them.

#### Fixed

- `{"ns.coll": "c"}` after a change stream read with mongod's plain filter
  (hash 90A52575) instead of mongod's rewritten one (D801EB4B). An `$or` of
  one value's equalities now prints as its `$eq`, and `$in`s on one path
  are no longer merged without an equality, as mongod's optimizer does.

Fixes RM-698, part (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_a_match_on_the_namespace_joins_the_oplog_filter)
