### Rust MongoDB server: a `$jsonSchema` query has a `queryShapeHash`

A `find`, `distinct` or `aggregate` that filters with `$jsonSchema` now
gets mongod's `queryShapeHash` in its `explain`, slow-query line and
profile entry. The Rust MongoDB server used to print none. mongod shapes the
schema as the tree of `$_internalSchema*` predicates its JSON Schema parser
builds, unoptimized. The server now builds the same tree keyword by keyword,
following `json_schema_parser.cpp` from mongod 8.2.11.

#### Fixed

- `$jsonSchema` queries carry a `queryShapeHash`, the same as mongod
  8.2.11's for each of 32 schemas covering every keyword.
  Fixes RM-358, in part (tests/backlog/test_rm_355_364.py::test_a_query_of_every_kind_names_its_query_shape;
  tests/mongod_live/test_query_shape_hash.py::test_a_json_schema).
- `setQuerySettings` for a `$jsonSchema` query is kept, as on mongod, where
  it was refused 115 because the server could not shape the query.
  Fixes RM-418, in part (tests/backlog/test_rm_219_diag_query_settings.py::test_setting_for_a_query_the_server_does_not_shape).
