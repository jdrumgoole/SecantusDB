### A command that never acquired its collection names `<db>.$cmd` on its line

A `create` of a collection that already exists with the same options, and a
`findAndModify` refused as it parses (FailedToParse: neither `update` nor
`remove`), end before mongod acquires the collection: mongod 8.2.11's
`Slow query` line names `<db>.$cmd` and its profiler writes no entry.

#### Fixed

- Both lines now name `<db>.$cmd`, and neither command is profiled.
  Fixes RM-595 (tests/backlog/test_rm_found_2026_11_13_rm495.py::test_a_command_refused_before_it_acquires_its_collection_names_cmd)
