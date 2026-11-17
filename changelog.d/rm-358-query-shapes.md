### More queries carry mongod's `queryShapeHash`

The Rust MongoDB server now shapes three more query forms the way mongod
8.2.11 does, so their slow-query line, profile entry and `explain` print the
same `queryShapeHash`.

#### Fixed

- A `$lookup` whose `from` is `{db, coll}` in `local` or `config` keeps the
  namespace form in its shape.
- `$out` to a time-series collection carries its `timeseries` options.
- A `$changeStream` with `showMigrationEvents`, `showSystemEvents`,
  `allowToRunOnConfigDB`, `allowToRunOnSystemNS` or
  `showRawUpdateDescription` is shaped, its flags in mongod's order.

Fixes RM-358 in part (tests/mongod_live/test_query_shape_hash.py::test_a_stage,
tests/mongod_live/test_query_shape_hash.py::test_a_change_stream); the
`$search` / `$vectorSearch` inputs of hybrid search stay open.
