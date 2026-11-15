### Rust MongoDB server: a `$near` read explains as GEO_NEAR_2D / GEO_NEAR_2DSPHERE

`explain` of a `$near` or `$nearSphere` read over a geo index now names
mongod's ring-search stage instead of a COLLSCAN.

#### Fixed

- The planner plans a `$near` / `$nearSphere` on a field a `2d` or
  `2dsphere` index leads as GEO_NEAR_2D or GEO_NEAR_2DSPHERE (a `2d` index
  first), with the query's other clauses a FETCH filter above it, as
  mongod 8.2.11 does (`FETCH {z: {$eq: 1}} <- GEO_NEAR_2D`; `SORT <-
  GEO_NEAR_2D` under a sort). The stage's executionStats are still not
  mongod's (RM-707).

Fixes RM-578 (tests/backlog/test_rm_found_rm472.py::test_a_near_read_explains_as_geo_near_2d)
