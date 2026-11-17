### A `$changeStream` of a view writes no `Slow query` line

The Rust MongoDB server refuses a `$changeStream` of a view (166) as
mongod 8.2.11 does, and now, like mongod, writes no `Slow query` line for
the refusal.

#### Fixed

- A `$changeStream` of a view no longer writes a `Slow query` line.
  Fixes RM-593 (tests/backlog/test_rm_view_namespaces.py::test_a_change_stream_of_a_view_writes_no_line)
