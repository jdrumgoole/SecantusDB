### Geo reads come back in mongod's order

The Rust MongoDB server now returns `$geoWithin`, `$geoIntersects`, `$near`
and `$geoNear` results in the order mongod 8.2 does. It transcribes mongod's
S2 and geohash machinery: the region coverer, the cells a `2dsphere` or `2d`
index keys each document by, the query's key intervals, and the near stage's
widening rings with its libc++ priority queue (with fused multiply-adds where
the arm64 reference build fuses them).

#### Fixed

- Results come in index-scan order under a geo index, and documents at one
  distance from a `$near` come in mongod's queue order. Fixes RM-041
  (tests/backlog/test_rm_029_056.py::test_geo_within_in_2dsphere_scan_order,
  ::test_geo_within_in_2d_scan_order, ::test_near_ties_in_scan_order).
- A hint to a geo index reads what the index holds, in its order. A `$near`
  or leading `$geoNear` hinted to `$natural`, to a missing index or to an
  index that cannot serve it gets mongod's 2 or 291. Fixes RM-044
  (::test_hinted_2dsphere_index_order, ::test_near_hinted_to_another_index).
- `$geoIntersects` with no sort over a `2dsphere` index returns mongod's
  order. Fixes RM-010
  (tests/backlog/test_rm_001_028.py::test_geo_intersects_unsorted_order).
- A bounded `$geoNear` reads only candidate documents again, through a
  superset that keeps lines and polygons reaching into the distance with
  every vertex outside. A legacy point on the sphere reads its
  `maxDistance` as radians. Fixes RM-048
  (crates/secantus-storage-adapter/tests/geo_order_wt.rs::a_bounded_geo_near_keeps_a_shape_whose_vertices_are_outside).
