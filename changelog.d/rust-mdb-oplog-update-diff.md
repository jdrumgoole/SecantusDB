### Rust MongoDB server: updates in `local.oplog.rs` are mongod's `$v: 2` diff, and `$[]` change events

#### Fixed

- An update in `local.oplog.rs` is now mongod 8.2.11's own `$v: 2` diff:
  `u`, `i` or `d` by whether the field existed, `s<field>` into
  sub-documents, `{a: true, u<i>}` into arrays, and a whole array where
  an operator rewrote it. It showed this server's change-stream
  description. Pipeline updates use mongod's document comparison. In
  both cases the diff is stored beside the description the change
  streams and replay read. 39 operator updates and 8 positional ones
  match mongod.
- An update through `$[]` or `$[<id>]` that changes more than one element
  is reported as the whole array in a change event's `updatedFields`, as
  mongod does; it was listed element by element. One changed element is
  still reported alone.

RM-224 part (`tests/backlog/test_rm_219_apply_ops.py::test_oplog_update_entries_are_mongods_diff`).
