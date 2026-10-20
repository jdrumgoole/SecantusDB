### The Rust MongoDB server words a query it cannot plan the way mongod does

When mongod's planner cannot plan a query (a hint naming no index, or a
collection scan under `notablescan`), its error prints the query it was
planning: the namespace, the normalised query tree, the sort, the
projection and the collation. The Rust MongoDB server now prints the same
text, rendered from the parsed query it already reproduces for `explain`,
including the bucket-level query mongod derives for a time-series
collection. `setParameter` now also changes the two SCRAM iteration counts,
and the counts govern the credentials `createUser` and `updateUser` write
afterwards.

Reproducing that text turned up several places where `explain`'s
`parsedQuery` was not mongod's, and they are fixed too: repeated clauses are
kept once, an `$or` of equalities on one path becomes its `$in` wherever it
stands, `{$eq: /x/}` stays an equality to the regex (bounded by one point),
and an aggregate takes a `$project` and the paging behind it into its query.

#### Added

- `secantus-commands` `explain/debug_string.rs`: mongod's
  `MatchExpression::debugString` and `CanonicalQuery` text, measured over
  about 130 shapes on mongod 8.2.11.
- `setParameter`: `scramIterationCount` and `scramSHA256IterationCount`
  (an int32 of at least 5000). Fixes RM-035
  (tests/backlog/test_rm_found_2026_10_15.py::test_scram_iteration_count_is_settable_at_runtime,
  tests/backlog/test_rm_planner_text.py::test_notablescan_refusal_prints_the_query_tree).

#### Fixed

- A hint naming no index, on `find`, `count`, `distinct`, `aggregate`,
  `update`, `delete`, `findAndModify` and `explain`, is refused in mongod's
  planner's words, with what each command prints of its paging, projection
  and collation (formerly RM-051, an accepted difference;
  tests/backlog/test_rm_planner_text.py::test_unknown_hint_is_refused_in_the_planners_words).
- On a time-series collection the refusal names `system.buckets.<name>` and
  the bucket-level predicates mongod derives (`control.min` / `control.max`,
  the bucket `_id` for the time field, `meta`). Fixes RM-089
  (tests/backlog/test_rm_085_112.py::test_an_unknown_hint_on_a_time_series_collection,
  tests/backlog/test_rm_planner_text.py::test_time_series_hint_refusal_prints_the_bucket_query).
- `notablescan`'s refusal prints mongod's query tree (RM-035).
- `explain`'s `parsedQuery`: repeated clauses kept once; `$or` equalities
  on one path folded into `$in` at any depth, nested `$or` flattened; a
  `$type` code written as a double is a whole number; `$alwaysTrue` dropped;
  `$not` of a regex and of a one-value `$in`; a BinData `$bits*` mask as bit
  positions; `{$eq: /x/}` kept as an equality, bounded by `[/x/, /x/]`; an
  aggregate's `$project` taken into the query with the `$skip` / `$limit`
  behind it. Fixes RM-195
  (tests/backlog/test_rm_planner_text.py::test_parsed_query_follows_mongods_optimiser,
  ::test_equality_to_a_regex_bounds_one_point,
  ::test_aggregate_takes_a_projection_and_the_paging_behind_it).

- `mapReduce` with a named `out` is refused as a `--noscripting` mongod
  8.2.11 refuses it (31264; it answered 9 with this server's own text). RM-027
  is closed as Joe's decision of 2026-10-16 (no JavaScript engine), pinned by
  tests/backlog/test_rm_001_028.py::test_commands_mongod_runs; RM-060
  (`$where` in an update's query) likewise, by
  tests/backlog/test_rm_057_084.py::test_where_in_an_update_query.

- `explain`: a comparison to an array bounds the index as mongod's does
  (`[MinKey, [ 5 ]]` for `$lt`, from the first element for `$gt`), and
  `explain` of `mapReduce` is refused as a `--noscripting` mongod refuses it
  (31264, it answered 20). Fixes RM-074
  (tests/backlog/test_rm_057_084.py::test_explain_leftovers,
  ::test_where_and_map_reduce_explain_without_javascript).
- `$listLocalSessions` / `$listSessions` know whose session is whose: each
  `_id.uid` is the SHA-256 of the `user@db` that used it, `{}` lists the
  caller's own, `{allUsers: true}` everyone's and `{users: [...]}` those
  users'. Fixes RM-156
  (tests/backlog/test_rm_057_084.py::test_list_local_sessions_lists_the_callers_by_user_hash).

#### Known gaps

- `$jsonSchema` is echoed in `parsedQuery` and the query tree where mongod
  translates it (RM-196).
