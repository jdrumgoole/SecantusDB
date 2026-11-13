### Rust MongoDB server: a refused write of a view logs its statements

An `update` or `delete` of a view now writes a `Slow query` line for each
statement it refuses, before the command's line, as mongod 8.2.11 does.

#### Fixed

- An `update` or `delete` of a view, refused with 166
  `CommandNotSupportedOnView`, wrote only the command's line. mongod
  acquires the namespace for each statement it tries, finds a view,
  refuses the statement and writes a line for it (`<db>.v`, `"normal"`,
  `ok: 0`, the refusal, with `multi` and `upsert` filled in on the
  statement). It writes one line for an ordered batch, which stops at its
  first statement, and one per statement for an unordered batch. This
  server now writes the same lines and takes the statement's write round
  (`Database` and `Collection` w, a flow control ticket). A non-retryable
  command's lines therefore count mongod's locks. An `insert` of a view
  still writes no statement line, as on mongod.
  Fixes RM-487 (tests/backlog/test_rm_view_namespaces.py::test_a_write_to_a_view_has_a_statement_line,
  tests/backlog/test_rm_view_namespaces.py::test_an_unordered_write_to_a_view_has_a_line_per_statement).

#### Filed

- RM-589: these lines still count fewer locks than mongod's for a
  retryable write (one transition lock short), for an `insert` (no write
  round) and for a statement of a transaction (none), because the server
  refuses the write before the point where mongod does.
