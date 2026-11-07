### Writes say how long they held their oplog slot

The Rust MongoDB server now prints `totalOplogSlotDurationMicros` on the
`Slow query` line and in the profile entry of a write, as mongod 8.2 does.
The field shows how long the write kept its oplog timestamp open before it
committed. While a slot is open it is a hole in the oplog: change streams
and majority reads cannot move past it. A write that holds one too long
holds every reader back.

The server measures this the way mongod does. Each run of oplog entries a
write mints is timed from the moment it is minted until the transaction
that wrote it commits, and the times add up. The field appears on inserts,
update and delete statements and their commands, `findAndModify`, and on
`create`, `drop`, `collMod`, `dropIndexes`, `renameCollection` and an
`aggregate` with `$out`. A write that minted nothing has none: an update
that matched or changed nothing, or a statement inside a transaction,
whose slots are counted on its `commitTransaction`. A write that rolled
back, such as a duplicate-key insert, has none either. This matches what
mongod printed for each of those cases.

#### Fixed

- `secantus-storage`: each in-flight oplog run records when it was minted.
  When the run's transaction commits (a statement, a user transaction or
  an autocommitted emit), the time it was held is credited to the running
  operation's `OperationCounts::oplog_slot_micros`. An operation inside
  another adds its time to the outer one, as it does for its index keys.
  The PostgreSQL server links the same storage crate; nothing it prints
  reads the new count.
- `secantus-commands`: `totalOplogSlotDurationMicros` goes on the command's
  line and profile entry, and on each update or delete statement's line and
  entry, in mongod's place in each. It is printed only when greater than
  zero. The profile entry stores it as mongod's `appendNumber` does: int32
  within 2^30, int64 above.
- `tests/mongod_live/test_slow_query_lines.py` now compares the field's
  presence across every scenario, instead of leaving it out.
- Fixes RM-361
  (tests/backlog/test_rm_355_364.py::test_a_write_says_how_long_it_held_its_oplog_slot,
  crates/secantus-storage/tests/oplog.rs::a_write_counts_how_long_it_held_its_oplog_slots).
- Filed RM-378: the `Slow query` line of a `renameCollection` names
  `admin.<db>.c` where mongod names `<db>.c`.
