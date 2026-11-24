### Rust MongoDB server: a hint to a wildcard index no predicate can use is refused

`secantusd-mdb` now refuses, as `mongod` 8.2.11 does, a read hinted to a
wildcard index when no predicate of the query bounds a path the index keys,
since `mongod` will not build a whole-index scan of one. Before, it scanned
the whole index (`{"w.a": {$ne: 3}}`, a `$geoWithin` or an empty filter
hinted to `$**` answered every document).

#### Fixed

- The refusal is 291 `NoQueryExecutionPlans` in `find`, `count`, `distinct`,
  `aggregate`, `explain`, `update`, `delete` and `findAndModify`, "$hint:
  refusing to build whole-index solution, because it's a wildcard index" when
  the query names one path the index expands to, and "No query solutions" when
  it names none or several (an empty filter, `_id`, a path a
  `wildcardProjection` leaves out, `$size`, a bit test, two negations). A
  query some predicate bounds (an equality, range, `$in`, `$exists: true`,
  `$type`, `$regex`, `$mod`, `$elemMatch`, `$expr`, every branch of an `$or`)
  is planned, and a partial wildcard index the query does not imply is
  refused as one no predicate bounds.
- Fixes RM-781 (`tests/backlog/test_rm_geo_wildcard_plans.py::test_a_hint_to_a_wildcard_index_no_predicate_can_use_is_refused`).
