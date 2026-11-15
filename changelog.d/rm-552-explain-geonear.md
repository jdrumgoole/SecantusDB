### Rust MongoDB server: `explain` of a `$geoNear` pipeline runs

An `explain` of `aggregate` with `$geoNear` failed with 26, because its
argument check runs against a namespace with no geo index. It now explains
the pipeline and prints its `queryShapeHash`, as mongod 8.2.11 does.

#### Fixed

- Fixes RM-552 (tests/backlog/test_rm_found_rm358.py::test_an_explain_of_geo_near_runs)
