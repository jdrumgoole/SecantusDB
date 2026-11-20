### A non-logged oplog is refused where it would lose acknowledged writes

`--oplog-nonlogged` (`oplog_nonlogged=True`, `SECANTUS_OPLOG_NONLOGGED`) on a
store whose data tables are not logged, which is the default, left nothing
in WiredTiger's log: that store recovers its data by replaying the logged
oplog, and the flag took the oplog out of the log too. A crash then lost
every write since the last checkpoint, `j: true` acknowledged or not. On
2026-11-20, 2,000 acknowledged inserts all disappeared after a SIGKILL.
mongod journals its oplog always. The server now refuses the combination
when the store is opened, with a message that names `--data-logged`.

#### Fixed

- The storage layer refuses a non-logged oplog on a store whose data tables
  are not logged. A new store is refused before WiredTiger writes a file of
  it, and an existing one by the mode recorded in it. With `--data-logged`
  the flag works as before: only the oplog tail since the last checkpoint is
  at risk, and the data is not.
  Fixes RM-731 (tests/backlog/test_rm_found_2026_11_20_unlogged_oplog.py::test_a_non_logged_oplog_keeps_or_refuses).
- The storage tests that open a store as the server does, with unlogged data
  tables, ask for a logged oplog explicitly. The `storage-async` CI lane sets
  `SECANTUS_OPLOG_NONLOGGED=1` for the whole suite, so those tests had been
  running the unsafe combination there.
- `docs-rust/embedded.md` said the wrong store keeps the synchronous oplog.
  It is the store with unlogged data tables, the default, whose writes carry
  their oplog timestamps.

The storage crate is shared: the PostgreSQL server links this change, though
it does not use the oplog modes.
