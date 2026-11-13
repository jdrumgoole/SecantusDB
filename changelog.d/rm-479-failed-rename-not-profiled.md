### Rust MongoDB server: a failed `renameCollection` is no longer profiled

mongod 8.2.1 profiles a `renameCollection` that renamed under its source
collection, and one that failed nowhere: a failed rename's namespace is
the command's own `admin.$cmd`, and `admin` profiles no command. The Rust
MongoDB server profiled a failed rename in the source's database as well.

#### Fixed

- A `renameCollection` that fails -- a source that does not exist, a
  target that does, a rename to itself, a bad target name -- writes no
  `system.profile` entry.

Fixes RM-479 (tests/backlog/test_rm_slow_query_namespace.py::test_a_failed_rename_is_not_profiled)
