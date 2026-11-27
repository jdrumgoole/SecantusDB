### A `$lookup` that mongod joins by a nested loop is explained as SBE

`explain` of a `$lookup` on `localField` / `foreignField` that no index serves
and that mongod does not join by hash (a command with `allowDiskUse: false`,
or a foreign collection past 10,000 documents or 100 MiB) now reports what
mongod 8.2.11 does: `explainVersion` "2", an `EQ_LOOKUP` of strategy
`NestedLoopJoin` over the query's plan, and the slot-based plan it runs: `nlj`
over the local rows with, for each, a `scan` of the whole foreign collection
under the `filter`, `group` and `union` that keep the documents whose keys
match. `executionStats` counts what the scan read and the filter tested for
each local row.

`collectionScans` of a join counts each time a scan was opened, as mongod's
does, not each scan stage in the plan.

#### Fixed

- A nested-loop `$lookup` was explained as a classic `$cursor` and `$lookup`
  stage. Fixes RM-683
  (tests/backlog/test_rm_lookup_sbe.py::test_a_lookup_on_a_large_foreign_collection_is_a_nested_loop_join,
  tests/mongod_live/test_lookup_nested_loop_join.py); the one strategy left,
  `DynamicIndexedLoopJoin`, is filed as RM-840.
- A `$lookup` over a collation-bearing index and a collection past 10,000
  documents was explained as a hash join; mongod joins it by a dynamic
  indexed loop, so it is explained as the classic stage until that is built
  (RM-840).
