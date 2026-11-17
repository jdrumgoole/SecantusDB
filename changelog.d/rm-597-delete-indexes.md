### `deleteIndexes` runs as `dropIndexes`'s alias

The Rust MongoDB server now accepts `deleteIndexes`, which mongod 8.2 runs as
an alias of `dropIndexes`, instead of refusing it as an unknown command.

#### Fixed

- `deleteIndexes` drops indexes as `dropIndexes` does, and refuses its fields
  under the name `dropIndexes`, as mongod 8.2.11 does.
  Fixes RM-597 (tests/backlog/test_rm_found_2026_11_13_rm495.py::test_delete_indexes_is_drop_indexes)
