### The Rust MongoDB server caches the plans of queries mongod runs in SBE

When a `$group` or `$lookup` can be pushed into the query behind a `$match`, mongod 8.2 runs that front in its slot-based engine (SBE). It still plans it with the classic planner, but leaves index intersections out of the candidates. It caches the winner under a key ending `s`, with `worksType: "reads"` and `works` set to the keys and documents the winning plan read in its trial. An active entry's plan is kept as long as it finishes before its reads reach ten times that number. `count_documents` sends such a pipeline. The Rust MongoDB server cached none of these queries. It now plans, caches, reuses and replans them as mongod does, and `$planCacheStats` lists the same entries, field for field.

A rooted `$or`'s branches are cached one by one. mongod keys each branch, and records it in `createdFromQuery`, with the query's projection, whether the query is a `find` or an aggregate's front. The branches are now keyed and listed that way here too.

#### Fixed

- A `$match` in front of a `$group` or `$lookup` that mongod pushes into SBE is planned without index intersections, so the candidates match, in the cache entry and in `explain`'s `rejectedPlans`. It is cached under its `s` key with `worksType: "reads"`. Its cached plan is replanned when its reads reach ten times the entry's.
- A rooted `$or`'s branch entry is keyed with the query's projection and lists it in `createdFromQuery`.
- Fixes RM-340 (tests/backlog/test_rm_340_342_plan_cache_gaps.py::test_a_query_sbe_runs_is_cached_with_its_reads, ::test_a_cached_sbe_plan_is_replanned_at_ten_times_its_reads, ::test_a_query_sbe_runs_tries_no_index_intersection).
- Fixes RM-430 (tests/backlog/test_plan_cache_or_branches.py::test_an_or_branch_is_keyed_with_the_projection).

#### Known gaps

- RM-429: a rooted `$or` in front of stages SBE runs still caches no branch. mongod's key for that branch is not yet reproduced (tests/backlog/test_plan_cache_or_branches.py::test_an_or_branch_before_sbe_stages_is_cached).
