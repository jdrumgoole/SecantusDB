### Rust MongoDB server: a rooted `$or`'s branches are cached in front of SBE stages

A rooted `$or` in front of stages mongod runs in SBE (a `$group`, say) now
caches each branch the way mongod 8.2.11 does: a classic entry
(`worksType: "works"`) under a key whose shape ends in `o`, the flag mongod's
`encodeClassic` gives a subplanner's branch of an SBE-compatible query, so it
never shares an entry with a classic query of the same clause.

#### Fixed

- `$planCacheStats` lists the branch entries of such a pipeline, with
  mongod's `planCacheShapeHash` and `planCacheKey`.

Fixes RM-429 (tests/backlog/test_plan_cache_or_branches.py::test_an_or_branch_before_sbe_stages_is_cached)
