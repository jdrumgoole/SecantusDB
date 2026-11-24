### `explain` plans `$geoWithin`, `$geoIntersects` and wildcard predicates from their indexes

`secantusd-mdb` now plans a `$geoWithin` / `$geoIntersects` over a `2d` /
`2dsphere` index, an equality, range, `$in`, `$exists: true`, `$type` or
`$regex` over a wildcard index, and a compound `$near`'s equalities, as
`mongod` 8.2.11 does: the same stages, indexes, bounds, multikey paths, rejected
plans and keys examined. Before, each was a collection scan in `explain`, and
a wildcard index named in a `hint` read as an error.

#### Added

- A `2d` index's covering of a `$geoWithin` shape (its geohash intervals,
  printed `BinData(128, ...)`) and a `2dsphere` index's S2 cell intervals
  are the bounds of a FETCH over IXSCAN; the exact test stays the FETCH's
  filter. A `2d` index serves only the legacy shapes, and not `$geoIntersects`,
  as `mongod`'s does.
- A wildcard index is planned one queried path at a time, as `mongod` expands
  it: an IXSCAN keyed `{$_path: 1, <path>: 1}` with `$_path` bounded to the
  path (and to the paths below it for `$exists: true`), `multiKeyPaths` from
  the arrays on the way to the path, the paths tried in `FieldRef` order, no
  intersection of two entries of one index, and the index's `wildcardProjection`
  deciding which paths it serves. `$ne`, `$nin`, `$not`, `$exists: false`
  and an equality to `null` or to a non-empty object or array are not served.
  The multikey paths of a wildcard index are recorded in the registry as a
  `2dsphere` index's are.
- A compound `$near` with an equality (or a range) on a field of its index
  other than the geo one is the near stage alone; the field before the
  geo one may be the equality.

#### Fixed

- A `2d` index reported itself multikey, with `multiKeyPaths`, when a
  document held a location pair; `mongod` tracks no paths for one.
- Fixes RM-777 (`tests/backlog/test_rm_storage_model.py::test_geo_within_and_wildcard_indexes_are_planned`,
  `tests/mongod_live/test_geo_wildcard_plans.py`). Files RM-779 (a `$near`
  with a range on a compound index's leading field returns too few
  documents), RM-780 (a `$near`'s `executionStats` leave out its inner
  scans) and RM-781 (a hint to a wildcard index no predicate can use is
  not refused).
