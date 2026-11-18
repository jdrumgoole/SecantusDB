### Built-in functions carry their initdb descriptions

`obj_description('abs(int)'::regprocedure, 'pg_proc')` answers `absolute
value`, as on PostgreSQL, whose initdb writes every built-in routine's
description into `pg_description`; psql's `\df+` and any tool reading
`pg_description` now see them. A COMMENT ON FUNCTION replaces one, and
COMMENT ... IS NULL deletes it.

#### Fixed

- `pg_description` holds initdb's 3,228 built-in routine descriptions,
  dumped from PostgreSQL 15.19.
  Fixes RP-969, in part (tests/backlog/test_rp_found_rp514.py::test_builtin_routine_has_initdb_description, tests/backlog/test_rp_found_rp514.py::test_comment_replaces_and_removes_initdb_description)
