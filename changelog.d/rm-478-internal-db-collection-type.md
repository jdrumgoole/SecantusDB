### Rust MongoDB server: a `Slow query` line on `admin`, `local`, `config` or a `system.` collection names its kind

A `Slow query` line's `collectionType` now names a namespace the way
mongod 8.2.11 does, from its name, before it looks at what is behind it.

#### Fixed

- A line for an operation on an internal database said `collectionType:
  "normal"`. mongod's says `"admin"`, `"local"` or `"config"`, whatever the
  operation did: a `ping` (`admin.$cmd`), a read of a missing or present
  collection, an `insert`, a `find` that failed, and a failed
  `renameCollection`, whose line has named `admin.$cmd` since RM-378. The
  index build thread's line on one says the same.
- A line for a `system.` collection, in any database (`admin.system.version`,
  `config.system.sessions`, `<db>.system.js`), said `"normal"`, and a read
  of `<db>.system.views` said `"view"`; mongod's says `"system"` for each.
  This is the first step of the same check: `admin.system.version` is
  `"system"`, not `"admin"`.
- A read of a view or a time-series collection that succeeds still says
  `"view"` / `"timeseries"`, on an internal database too.
- Fixes RM-478 (`tests/backlog/test_rm_slow_query_namespace.py::test_a_line_on_an_internal_database_names_its_kind`,
  `tests/backlog/test_rm_slow_query_namespace.py::test_a_line_names_its_kind_whatever_the_operation_did`).
- Filed RM-581: an `update` or `delete` statement's line leaves out its
  command's `comment` (`tests/backlog/test_rm_statement_line_comment.py`).
