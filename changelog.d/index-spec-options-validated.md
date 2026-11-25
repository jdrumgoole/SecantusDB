### `createIndexes` validates the options of an index spec as mongod does

The options of an index spec were checked in an order of their own, some not
at all. They are now read as mongod 8.2.11 reads them, over every option and
value class: an unknown field first, then each field in the order the spec
gives them, then the checks mongod makes once the fields are read.

#### Fixed

- `weights` on a key that is no text key (67), `wildcardProjection` on a key
  that is no wildcard key (2, "is only allowed in 'wildcard' indexes") or no
  top-level `$**` key (9), and an empty one (9) are refused with mongod's
  words. `prepareUnique` is no unknown field.
- A wrong type is 14 with the field's own message for `weights`,
  `default_language`, `language_override`, `textIndexVersion`,
  `2dsphereIndexVersion`, `bits`, `min`, `max`, `bucketSize`,
  `coarsestIndexedLevel`, `finestIndexedLevel` and `v`, and a spec with two
  faults answers with the first one mongod reaches (a bad key answers where
  the key stands in the spec).
- A `storageEngine` option is checked as mongod checks it: not a document or
  empty (67), a name that is no engine (72), `wiredTiger` not a document (2),
  an option other than `configString` (72), a `configString` that is no
  string (14) or that WiredTiger's own configuration check refuses (2, with
  its message).
- `$listCatalog` shows an index's spec as the catalog keeps it, with the
  options its kind of index does not use, where `listIndexes` shows only the
  ones it does.

Fixes RM-771 (tests/backlog/test_rm_found_rm732.py::test_create_indexes_validates_its_options_as_mongod_does)
