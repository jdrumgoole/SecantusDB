### Rust MongoDB server: `$planCacheStats` absorbs a later `$match` as mongod does

`$planCacheStats` now absorbs, in place of the `$match` right after it, each
later `$match` a stage swaps ahead of itself, as mongod 8.2's
`doOptimizeAt` does. The run shares `explain`'s absorption, so the two agree.

#### Fixed

- `[{$planCacheStats: {}}, {$match: {isActive: false}}, {$project: ...},
  {$match: {works: {$gte: 0}}}]` returned no entries; it returns every entry,
  the first filter replaced, as on mongod 8.2.11.

Fixes RM-554 (tests/backlog/test_rm_found_rm466.py::test_a_later_match_replaces_the_one_plan_cache_stats_absorbed)
