### Dropping a view is profiled

A `drop` of a view now writes its profile entry, as mongod 8.2.11 does:
`{op: "command", ns: "<db>.v", command: {drop: "v"}}`, the same shape as a
collection's drop. The profiler took the view's `{ns, ok}` reply, which
carries no index count, for a drop of a collection that does not exist,
which mongod does not profile.

#### Fixed

- Rust MongoDB server: a `drop` of a view writes a profile entry.
  Fixes RM-384 (tests/backlog/test_rm_drop_reports.py::test_dropping_a_view_is_profiled)
