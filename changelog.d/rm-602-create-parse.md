### Rust MongoDB server: a malformed `create` is refused as it parses

A `create` with an unknown or mistyped field, or a `size` / `max` out of
range, is now refused before it runs, as mongod 8.2 refuses it: it is not
a connection's first command, and writes no `Slow query` line or profile
entry. Its fields are checked in document order, as mongod's IDL parse
checks them. A `listDatabases` on a database other than `admin` is still
logged, but is no longer counted as a connection's first command.

#### Fixed

- Fixes RM-602 (tests/backlog/test_rm_found_rm386.py::test_create_and_db_stats_refused_as_they_parse_never_run)
