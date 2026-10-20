### Rust MongoDB server: seven more administrative commands

#### Added

Seven commands that answered 59 CommandNotFound now answer as mongod 8.2.11
does on a one-member replica set. Each answer is true of this server: it
keeps no egress connection pool, holds no storage resources on an idle
cursor, and has never rolled back.

- `invalidateUserCache`.
- `replSetGetRBID` (rollback id 1).
- `replSetFreeze` and `replSetMaintenance`: 95, the refusal a primary
  gives.
- `dropConnections`.
- `releaseMemory`, which lists the cursors it let go; they still answer
  `getMore`.
- `filemd5`: the MD5 of a GridFS file's chunks, read in `n` order through
  an index. It carries mongod's errors for a missing index (291), chunks
  out of order (10040) and a chunk whose `data` is not binData
  (50849 / 50848). `partialOk` is refused by name, because its `md5state`
  is mongod's own MD5 state.

RM-219 part
(`tests/backlog/test_rm_found_2026_10_20b.py::test_short_admin_commands_answer_as_mongod`);
ten commands remain open under RM-219.
