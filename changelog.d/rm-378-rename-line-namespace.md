### A rename's log line names the collection it renamed

The `Slow query` line the Rust MongoDB server writes for a
`renameCollection` now names the namespace mongod 8.2 names. The command
runs against `admin` and takes a full namespace as its argument, and the
line used to join the two into `admin.<db>.c`; it now names the collection
it renamed, `<db>.c`, and a rename that failed -- a missing source, an
existing target, a view, a bad name -- names the command's own
`admin.$cmd`, as mongod's does.

#### Fixed

- A `renameCollection`'s `Slow query` line names the renamed collection,
  or `admin.$cmd` when the rename failed, instead of `admin.<db>.<coll>`.
  Fixes RM-378 (tests/backlog/test_rm_slow_query_namespace.py::test_a_rename_line_names_the_collection_it_renames)
