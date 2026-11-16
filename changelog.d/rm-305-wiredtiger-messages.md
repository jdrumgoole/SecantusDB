### Rust MongoDB server: WiredTiger's own startup messages reach the log

The server opens WiredTiger with mongod 8.2's message configuration
(`json_output=(error,message)` and its `verbose` list) and writes each
message WiredTiger produces while opening as mongod does: a
`WiredTiger message` (22430) line between `Opening WiredTiger` and
`WiredTiger opened`, with WiredTiger's JSON as `attr.message` and the
component named by its category (`WTRECOV`, `WTEVICT`, ...).

#### Fixed

- `getLog` and the server log now carry WiredTiger's startup messages.
  The storage crate's connection keeps the messages its event handler
  receives (the Rust PostgreSQL server links the change, and opens without
  the verbose configuration, so it writes nothing new).

Fixes RM-305 in part (tests/backlog/test_rm_001_028.py::test_startup_writes_wiredtigers_own_messages)
