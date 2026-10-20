### Rust MongoDB server: the 64-index limit

A collection now holds at most 64 indexes, `_id_` included, as on mongod
8.2.11.

#### Fixed

- `createIndexes` refuses an index past the limit with 67
  CannotCreateIndex. The text and wrapper follow mongod's:
  - plain when the collection is already full, or when the request created
    the collection;
  - under the index-build wrapper when the request itself fills the
    collection.

  A request that crosses the limit builds none of its indexes. An index
  that already exists still answers "all indexes already exist". Fixes
  RM-218 (`tests/backlog/test_rm_found_2026_10_20b.py::test_index_count_limit`).
