### `createIndexes` keeps `sparse: false`, as mongod does

A spec's options are stored as mongod stores them: `sparse: false` stays in
the catalog's spec and in `listIndexes`, and the options mongod parses away
do not.

#### Fixed

- `sparse: false` was dropped from the spec, as `unique: false` and `hidden:
  false` are. `mongod` 8.2.11 keeps it (on a plain, a unique, a TTL, a
  partial, a `2dsphere`, a wildcard and a hashed index), and `background` and
  `expireAfterSeconds: 0` with it.
- `prepareUnique: false` is accepted and dropped, where it was refused as an
  unknown field; `dropDups` and `ns` are dropped, where they were stored and
  listed.
- A spec with `sparse: false` is the index one without `sparse` is, and
  `background` takes no part in comparing two specs: asking for either again
  is "all indexes already exist", and `background: true` on an index that
  has none no longer raises `IndexKeySpecsConflict`.
- The specs a conflict error quotes list `unique`, `hidden` and
  `prepareUnique` before `key`, and the other options after `name`, in the
  order given, as mongod writes them.

Fixes RM-788 (tests/backlog/test_rm_index_spec_fidelity.py::test_create_indexes_keeps_a_false_sparse_as_mongod_does)
