### Hashed indexes

`secantusd-mdb` builds hashed indexes as `mongod` does. `createIndex({a:
"hashed"})` answered 67 CannotCreateIndex; an index is now built, listed and
kept up by every write, and it is an index `mongod` 8.2 validates and serves
from, and one this server serves from a store `mongod` wrote.

#### Added

- A hashed field's key is mongod's 64-bit hash of the value (MD5 over the
  BSON element, a missing field as null, numbers of every type squashed to
  one), written to the index table as the KeyString of a `NumberLong`. One
  hashed field may sit in a compound index with ascending or descending
  ones, under a collation (the hash is of the string's collation key), as a
  partial or sparse index, on `_id`, and on the `metaField` of a time-series
  collection.
- The planner reads a hashed index for an equality, an `$in`, a null and
  `$exists: false` (a point per hash value, an array operand bounded as on a
  plain index) and for `$exists: true`; never for a range, a negation, a
  regex or a type. It serves no sort, is intersected with other indexes'
  scans, and reports no `multiKeyPaths`. `explain`, its plan cache entries
  and `estimatedSizeBytes`, the document order of a hinted scan and a
  hinted sort match `mongod` 8.2.11 over every BSON type.
- The refusals are mongod's: `unique` 16764, two hashed fields 31303, a hashed
  index beside another index plugin, a hashed field on a time-series time
  field or measurement, and an array under a hashed field by an insert,
  update, replacement, upsert, `findAndModify` or index build (16766).

#### Fixed

- A `{$**: "hashed"}` key and a wildcard field beside another plugin are
  refused as mongod refuses them; an `_id` index of kind `hashed` is built.

Fixes RM-732 (tests/backlog/test_rm_hashed_index.py::test_a_hashed_index_is_built_and_serves_equality,
tests/backlog/test_rm_hashed_index.py::test_hashed_index_refusals_are_mongods,
tests/backlog/test_rm_hashed_index.py::test_a_compound_index_with_one_hashed_field_is_built,
tests/backlog/test_rm_hashed_index.py::test_hashed_index_plans_are_mongods,
tests/backlog/test_rm_hashed_index.py::test_hashed_index_keys_every_bson_type_as_mongod_does,
tests/backlog/test_rm_hashed_index.py::test_a_hashed_index_with_a_collation_hashes_collation_keys,
tests/backlog/test_rm_hashed_index.py::test_a_hashed_index_refuses_an_array_wherever_it_is_written,
tests/backlog/test_rm_hashed_index.py::test_hashed_index_specs_are_validated_as_mongod_validates_them,
tests/backlog/test_rm_hashed_index.py::test_a_time_series_collection_takes_a_hashed_index_on_its_meta_field).
