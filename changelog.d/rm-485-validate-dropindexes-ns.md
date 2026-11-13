### Rust MongoDB server: a `validate`, and a `dropIndexes` refused before it reaches its collection, log `<db>.$cmd`

A `Slow query` line for a `validate` named the collection or view it was
given. mongod 8.2.11's line names `<db>.$cmd` for every `validate`, because
its `validate` never takes the collection's namespace for the operation. The
same holds for a `dropIndexes` that is refused before it acquires its
collection.

#### Fixed

- A `validate` line names `<db>.$cmd`: of a collection, of a time-series
  collection, and of a view it refuses (CommandNotSupportedOnView).
- A `dropIndexes` of a view (CommandNotSupportedOnView) or of a collection
  that does not exist (NamespaceNotFound) is refused before it acquires a
  collection. Its line names `<db>.$cmd`, and it is no longer profiled; it
  was profiled under the missing collection. A `dropIndexes` refused once it
  holds its collection, for an index that does not exist or the `_id` index,
  still names the collection and is profiled, as mongod's is.
- Fixes RM-485 (tests/backlog/test_rm_view_namespaces.py::test_a_refused_validate_of_a_view_names_cmd,
  tests/backlog/test_rm_view_namespaces.py::test_an_unacquired_drop_indexes_names_cmd_and_is_not_profiled;
  crate test `slowop::tests::a_validate_and_an_unacquired_drop_indexes_name_cmd`).
- Filed while working it: RM-587 (a `dataSize` of a view answers `ok`) and
  RM-588 (a `dropIndexes` of `system.views` is refused). RM-495 now also
  covers a `compact` line.
