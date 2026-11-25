### `createIndexes` keeps `sparse` and `background` as given and reads every flag as mongod does

`createIndexes` turned every number in an index flag into a bool, kept a
string `background`, took `hidden: 1` and `hidden: "x"` as hidden indexes and
refused `prepareUnique: true`. The catalog now keeps what mongod 8.2.11 keeps
and the options are read as it reads them.

#### Fixed

- `sparse` and `background` stay as the request gave them in the catalog
  (`$listCatalog`, and the store mongod opens): `sparse: 1.0` is a double and
  `background: 1` an int, where `listIndexes` shows both as the bools they
  read as. Two specs that differ only in how they spell a sparse flag are the
  same index. A sparse index read from a store mongod wrote with `sparse: 1.0`
  is served as a sparse one.
- `unique` and `prepareUnique` are written as bools, and `v: 1.0` as the int
  `1`. A flag that is no number or bool (a string, null, an object) is
  refused (14), and `hidden` must be a bool: `hidden: 1` is refused ("The
  field 'hidden' must be a bool, but got int").
- `prepareUnique: true` is accepted at creation and arms the index, as a
  `collMod` arms one; with `unique` it is refused (67).
- `listIndexes` shows `bucketSize` as a double.

Fixes RM-791 (tests/backlog/test_rm_index_spec_fidelity.py::test_create_indexes_keeps_a_flag_as_given_and_refuses_a_hidden_that_is_no_bool)
