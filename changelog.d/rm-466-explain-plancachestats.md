### Rust MongoDB server: `explain` of `$planCacheStats` names its stage

An `explain` of an `aggregate` that starts with `$planCacheStats` failed
with 50933 on the Rust MongoDB server. It now lists the stage as mongod
8.2.11 does, with the `$match` mongod's optimizer folds into it.

#### Fixed

- `explain` of `[{$planCacheStats: {}}]` failed with 50933 (`collection
  '<db>.explain.dry.run' does not exist`): explain checks a pipeline's
  arguments by running it against a namespace of its own, which has no plan
  cache to read. The stage now goes through the path for stages that are
  their own source, and lists `{$planCacheStats: {allHosts: false}}`, then
  `queryShapeHash`, for a collection that exists or not.
- The `$match` mongod absorbs into the stage is listed as `match`, ahead of
  `allHosts`: the one right after it, as written, and, in place of that one,
  each later `$match` a stage swaps ahead of itself. A `$match` that
  `$project` or `$addFields` rewrites on the way is listed parsed. A
  `$match` right after an absorbed one stays where it is.
- `executionStats` counts the stage's entries through the absorbed
  `$match`, and fails for a missing collection with 50933 and mongod's
  message, without the executor's prefix.
- Filed RM-554: the stage itself (not its `explain`) keeps the first
  `$match` where mongod replaces it with a later one.

Fixes RM-466 (tests/backlog/test_rm_found_rm358.py::test_an_explain_of_plan_cache_stats_names_its_stage)
