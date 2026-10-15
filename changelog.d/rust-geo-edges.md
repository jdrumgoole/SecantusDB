### The geo edges match mongod

`2d` index options are refused and listed as mongod lists them. An index
build refuses what mongod's does, with its wording, and builds all of a
request's indexes or none. A `$near` centre outside a `2d` index's range is
16433, and a write whose `$near` cannot run changes nothing. `$geoNear`
inside `$lookup` and `$unionWith` fails under the outer command's prefix.
The sphere is tested as S2 tests it: points as leaf cells, a loop as its
smaller side, `strictwinding` big polygons, and a `$centerSphere` by its
complement.

#### Fixed

- A `2dsphere` `$geoWithin` through the index missed documents beyond the
  box of the polygon's vertices: edges bowing toward a pole, and polygons
  larger than a hemisphere.
- `delete` and `update` with a `$near` that had no usable index, or a centre
  outside the `2d` range, changed documents; mongod refuses them.
- `listIndexes` after a conversion to unique lists `unique` where mongod
  does. Fixes RM-067
  (tests/backlog/test_rm_057_084.py::test_list_indexes_key_order_after_conversion_to_unique).
- A `$geoNear` whose `query` has an unknown top-level operator is mongod's
  2, not the 291 for want of an index: mongod parses the query first.
- `bits: 20.0` on a `2d` index was read as the default 26. Existing `2d`
  indexes are rebuilt when the store opens.

Fixes RM-042, RM-043, RM-045, RM-047 (tests/backlog/test_rm_029_056.py;
crates/secantus-storage-adapter/tests/geo_edges_wt.rs).
