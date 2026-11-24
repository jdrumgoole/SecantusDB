### Rust MongoDB server: a `$near` no compound `2dsphere` index can plan is refused

`secantusd-mdb` now refuses, as `mongod` 8.2.11 does, a `$near` / `$nearSphere`
(and a leading `$geoNear`) over a compound `2dsphere` index whose geo field
does not lead when no predicate of the query bounds the index's LEADING field:
`{t: 1, pts: "2dsphere"}` with no predicate on `t`, a predicate on another
field only, or `t` under `$ne`, `$nin`, `$not`, `$exists: false` or an `$or`.
It answers `$exists`, `$in`, `$type`, a range and a hint, and every query a
compound `2d` index serves, as before.

#### Fixed

- The refusal is 291 `NoQueryExecutionPlans` in `find`, `count`, `distinct`,
  `explain`, `update`, `delete`, `findAndModify` and `aggregate`, worded
  "unable to find index for $geoNear query" when the leading field has no
  predicate the planner counts, and "No query solutions" when only a
  negation, an `$or` or the like touches it. Its `Tree:` is the query as
  `mongod` parses it (a `$not`, an `$in`, an `$or` printed as `mongod`
  prints them), not the clauses as written.
- `explain` of a `$near` with no geo index, or no usable one, failed to
  refuse; it does now.
- A `$geoWithin` or `$near` beside `$_internalExprEq` twins is printed in
  `mongod`'s order in a refusal's tree (`$expr`, geo, GEO_NEAR, the twins).
- Fixes RM-783 (`tests/backlog/test_rm_geo_wildcard_plans.py::test_a_near_over_a_compound_2dsphere_index_without_a_single_interval_is_refused`).
  Files RM-785 (a hinted `$near` over such an index is read nearest first,
  not in key order), RM-786 (a hinted `$geoNear` stage that mongod fails in
  its executor answers) and RM-787 (`explain` of a `$geoNear` stage lists a
  `$cursor` with an empty `parsedQuery`).
