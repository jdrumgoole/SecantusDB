### Rust MongoDB server: `applyOps`, and `local.oplog.rs` read newest first

#### Added

- `applyOps` applies oplog entries as mongod 8.2.11 does, so
  `mongorestore --oplogReplay` can replay into this server. It used to
  be 59 CommandNotFound.
  - Inserts, updates and deletes follow mongod's rules: an insert of an
    existing `_id` replaces the document, an update of a missing document
    does not apply unless `b: true`, and an update's `o` is mongod's own
    `$v: 2` diff (sub-document and array diffs included).
  - Command entries are applied when they would change something:
    `create`, `drop`, `collMod`, `dropIndexes`, `renameCollection` (an
    existing target is moved aside), `dropDatabase`, and a nested
    `applyOps`.
  - Errors are reported as on mongod. A malformed list refuses the whole
    command. An entry that raises stops the run, with `applied` and
    `results`. An entry that does not apply is `false` and the rest go on,
    ending in error 8 (52 cases against mongod, none different).

#### Fixed

- `find().sort({$natural: -1})` on `local.oplog.rs` now answers the newest
  entry. It answered the oldest, so a tool asking for the latest oplog
  entry (as `mongodump --oplog` does) got the first one ever written.

RM-219 part (`tests/backlog/test_rm_219_apply_ops.py::test_apply_ops_as_mongod`,
`::test_oplog_natural_descending_reads_the_newest_entry`). The oplog's
entry shapes are RM-224.
