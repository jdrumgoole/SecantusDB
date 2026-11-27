### An insert whose batch fails reports mongod's lock rounds

When a batch of several documents fails, mongod 8.2 inserts them again one
at a time. The Rust MongoDB server's slow-query line and profile entry now
count the same lock rounds and flow-control tickets for that retry: each
retry round ends at a failed document, an unordered batch starts a new
round for the documents after it, and a retryable write takes one more
session-row round for each document it inserts.

#### Fixed

- The `locks` and `flowControl` of an `insert` whose batch fails match
  mongod 8.2.11, ordered or not and retryable or not. This closes part of
  RM-357 (tests/backlog/test_rm_355_364.py::test_a_write_reports_mongods_locks_and_flow_control).
