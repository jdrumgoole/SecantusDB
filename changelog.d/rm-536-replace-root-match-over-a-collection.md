### Rust MongoDB server: a `$match` behind a `$replaceRoot` moves into a collection's query

Over a collection, a `$match` behind `{$replaceRoot: {newRoot: "$x"}}` (or
`$replaceWith: "$x"`) now moves ahead of it as mongod 8.2.11's optimizer
moves it: renamed onto `x`, in an `$or` with `{x: {$type: [4]}}` and `{x:
{$not: {$type: [3]}}}` so that what the `$replaceRoot` refuses still
reaches it, and the collection's query takes it (a rooted `$or`, planned as
`SUBPLAN`). A `$skip` or `$limit` behind the `$replaceRoot` goes ahead of
it into the query too. The rewrite ran only after a pipeline's own source
(`$documents`) before. A change stream's `$match` behind a `$replaceRoot`
now joins its event-type filter, as on mongod.

#### Fixed

- `explain` of `[{$replaceRoot: {newRoot: "$x"}}, {$match: {a: 1}}]` over a
  collection listed an empty `parsedQuery`, a bare `COLLSCAN` and the
  `$match` after the `$replaceRoot`; it now lists the `$or` as the query,
  `SUBPLAN` over the filtered `COLLSCAN`, and the `$replaceRoot` alone. The
  aggregate runs the moved `$match` as the query's filter, so it reads
  only the documents that pass.
  Fixes RM-536 (tests/backlog/test_rm_found_2026_11_10_explain_rewrites.py::test_a_match_moves_ahead_of_a_replace_root_into_the_query);
  regression tests in tests/mongod_live/test_replace_root_pushdown.py.
- Filed while working it: RM-658 (a FETCH's residual `$or` lists its
  branches in `parsedQuery`'s order) and RM-659 (`explain` with
  `executionStats` answers where a `$replaceRoot` fails).
