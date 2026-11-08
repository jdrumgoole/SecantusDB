### An aggregate's `$limit` and `$match` move through a projection as mongod moves them

mongod optimises a pipeline until nothing more moves. A `$match` goes ahead of the stages that leave the fields it reads, and a `$skip` or `$limit` goes ahead of the stages that keep the number of documents (`$project`, `$unset`, `$addFields`, `$set`), as far as the query that takes it. Each move can open the way for the other. So `[$project, $limit, $match]` reads as `[$limit, $match, $project]`: the query takes the limit, its plan projects only the fields the two remaining stages read, and explain lists both of them. The Rust MongoDB server made each move once and stopped, so it took the projection into the query and dropped it from the stage list. It now repeats the moves until nothing changes, as mongod does. The same change takes a `$limit` past an `$addFields`, keeps an exclusion projection out of the plan, and leaves a `$limit` where it is when an `$unwind`, or a `$match` behind an earlier `$limit`, stands between it and the query. The same shaping decides which stages the server runs after the read, in what order, so this changes how a pipeline runs but not what it returns.

#### Fixed

- `explain::shape::absorb` repeats `hoist_matches` and the new `pull_paging` until neither moves a stage. `pull_paging` moves a `$skip` / `$limit` ahead of `$project`, `$unset`, `$addFields`, `$set` and `$score`, but only while the query can take every stage ahead of it.
- `explain::hoist::hoist_matches` counts a `$skip` / `$limit` among the leading stages. A `$match` never moves past one, but a `$match` behind one now moves ahead of the stages after it.
- Fixes RM-308 (tests/backlog/test_rm_found_2026_10_24.py::test_explain_lists_the_projection_a_limit_moved_past)
- Fixes RM-369 (tests/backlog/test_rm_found_2026_10_24.py::test_an_inclusion_projection_keeps_its_int_flags_in_the_plan). The int flags were never the user's: they are the projection of what the `$match` and the kept `$project` read.
- Fixes RM-370 (tests/backlog/test_rm_found_2026_10_24.py::test_an_exclusion_projection_is_not_pushed_into_the_plan)
- Fixes RM-371 (tests/backlog/test_rm_found_2026_10_24.py::test_a_limit_between_a_reshape_and_a_match_is_still_taken)

#### Added

- `tests/mongod_live/test_explain_paging_shape.py`: 27 pipelines compared with mongod 8.2.11 (parsed query, plan stages with `transformBy`, stages after the read).
- Unit tests in `explain::shape` for the moves and for where a `$limit` stays.
