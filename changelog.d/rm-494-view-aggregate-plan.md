### Rust MongoDB server: an aggregate of a view names the plan beneath it

A `Slow query` line and profile entry for an `aggregate` of a view now
name the plan over the collection beneath the view, as mongod 8.2.11's do,
and carry that aggregate's plan cache hashes.

#### Fixed

- Over a view `{$match: {x: {$gte: 1}}}` on a collection indexed on `x`, an
  `aggregate` of the view (a `$group`, a `$match`, and the `getMore`s of
  its cursor) said `planSummary: "COLLSCAN"`, with `planCacheShapeHash`,
  `queryHash` and `planCacheKey` of its own. It was planned against the
  view, which has no indexes. mongod names `IXSCAN { x: 1 }` and prints the
  hashes of the same pipeline run on the collection. The line's plan is now
  that of the view's pipeline followed by the aggregate's, over the
  collection beneath, as a `find`, `count` or `distinct` of a view already
  was.
- Fixes part of RM-494
  (tests/backlog/test_rm_view_lines.py::test_an_aggregate_of_a_view_names_the_plan_beneath).
  Still open under RM-494: a `count_documents`, of a view or of a
  collection, runs as an `IXSCAN` that fetches every document where mongod
  runs a `COUNT_SCAN` that fetches none
  (tests/backlog/test_rm_view_lines.py::test_a_count_documents_runs_a_count_scan).
