### Rust MongoDB server: `getMore` checks its cursor id last, as mongod does

mongod's IDL parser reads the command-name field after every other field, so
a `getMore` with an int cursor id and no `collection` is refused for the
missing `collection` (40414), not for the id's type.

#### Fixed

- `{getMore: 1}` (and any wrong-typed id) without `collection` answers
  "BSON field 'getMore.collection' is missing but a required field", and an
  unknown field or wrong-typed `batchSize` / `maxTimeMS` is reported before
  the id's type, as on mongod 8.2.11.
  Fixes RM-606 (tests/backlog/test_rm_found_rm386.py::test_a_get_more_takes_an_int_cursor_id)
