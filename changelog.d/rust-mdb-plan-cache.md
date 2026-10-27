### The Rust MongoDB server keeps mongod's plan cache

When several indexes could answer a query, mongod tries them against each
other once for that query's shape and caches the winner. A query of the same
shape with other values then runs the cached plan without a new trial, as
long as the plan finishes within ten times the work it was cached with; one
that does worse sends the shape back to a trial. Which plan a query runs
decides the order its documents come back in, which document a
`deleteOne` removes, and which one an `updateOne` changes. The Rust MongoDB
server now keeps the same cache, with mongod 8.2's rules: an inactive entry
first, active once a later trial takes no more work, the bar doubled when
one takes more, an LRU of 5000 entries per collection, cleared when the
collection's indexes change.

`explain` reports the cache as mongod does: `queryHash`,
`planCacheShapeHash` and `planCacheKey`, the same values mongod prints for
the same query and indexes (its query-shape encoding and MurmurHash3,
transcribed), and `isCached` on the plan an active entry holds.
`$planCacheStats` lists the entries -- the cached plan, each candidate's
trial statistics and score -- and `planCacheClear` clears all of them or
one shape's.

#### Added

- `explain/plan_cache.rs`, `explain/cache_key.rs`: mongod's classic plan
  cache and its key. A read consults it before it is ordered
  (`order::consult`): `find`, the front of `aggregate`, `distinct`,
  `findAndModify`, `update`, `delete` and `count`. A branch of a rooted
  `$or` is cached on its own, as mongod's subplanner caches it.
- `$planCacheStats` (with the `$match` after it absorbed), and
  `planCacheClear` by shape. `planCacheSetFilter` and
  `planCacheClearFilters` clear the shape's entries.
- `explain`'s `queryHash`, `planCacheShapeHash`, `planCacheKey` and
  `isCached`.
- The trial's per-stage work accounting (`works`, `advanced`, `needTime`,
  `isEOF`, an index scan's `seeks` and duplicate checks), in
  `allPlansExecution` and the cache entries' `creationExecStats`.

#### Fixed

- The trial's tie-breaking is mongod's: the docs-examined, index-prefix and
  distinct-scan bonuses, in place of a fixed order of preference, and the
  tie-breakers' epsilon taken from a plan's advances, not its works.
  `explain`'s `score` is the score before the bonuses, as mongod's.
- An index intersection (`AND_SORTED`) is a candidate over a multikey
  index's point scan too, as mongod's is.
- A sort an index returns only by exploding its point bounds is not
  exploded behind a range on an earlier field: mongod sorts the scan's
  keys.
- An index scan whose first interval starts exclusive seeks past that
  value, as mongod's does, and counts a seek in the work that makes it.
- The trial stops at the query's limit only when it is under 101 documents.

Fixes RM-154 (`tests/backlog/test_rm_154_plan_cache.py`, every test) and
the `$planCacheStats` half of RM-215
(`tests/backlog/test_rm_154_plan_cache.py::test_plan_cache_stats_stage`).
Files RM-340 (a query SBE runs is not cached), RM-341 (no
`estimatedSizeBytes`) and RM-342 (no cache past 2,000 documents), with gap
tests in `tests/backlog/test_rm_340_342_plan_cache_gaps.py`. The trial's
explain is pinned by `tests/mongod_live/test_plan_trial.py`.
