### Rust MongoDB server: `queryFramework: "sbe"` runs an `aggregate` and a `distinct` in SBE

Under a `queryFramework: "sbe"` query setting mongod 8.2 (`trySbeEngine`)
runs a `distinct`, and an `aggregate`'s query even when no `$group` or
`$lookup` is pushed after it, in its slot-based engine. This server kept the
default `trySbeRestricted` rule for them and ran them classic, so their
expressions answered as the classic engine does (`$mod` by zero failed 16610
where mongod fails 4848403) and their `explain` was the classic one.

#### Fixed

- An `aggregate` under `"sbe"` runs its query in SBE, taking in an exclusion
  `$project` and the `$addFields` / `$set` stages that follow it; its
  `explain` is `explainVersion` "2" with the slot program mongod runs
  (a `PROJECTION_DEFAULT` of `makeBsonObj` for `$addFields` of constants
  and field paths), its `queryHash` and `planCacheKey` are mongod's, and a
  classic stage after it (`$unwind`) is listed after the `$cursor` with the
  counts mongod gives.
- A `distinct` under `"sbe"` runs its query in SBE and is explained and
  keyed as the `find` of that query.

Fixes RM-542 (tests/backlog/test_rm_found_2026_11_10_rm418.py::test_sbe_framework_pushes_an_aggregate_and_a_distinct);
pinned by tests/mongod_live/test_sbe_find_explain.py::test_sbe_aggregate_explain_matches_mongod,
::test_sbe_distinct_explain_matches_mongod and
::test_sbe_framework_runs_aggregate_and_distinct_in_sbe.
