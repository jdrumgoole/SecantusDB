### Rust MongoDB server: a standalone implies `w: 1` and refuses the default-concern commands

`secantusd-mdb --standalone` kept a replica-set member's write-concern
machinery. It now behaves as mongod 8.2.11 does as a standalone.

#### Fixed

- A write sent with no write concern takes the standalone's implicit
  `w: 1` and no longer waits for the journal (the extra transition IX and
  global IS lock rounds are gone from its `locks`).
- `getDefaultRWConcern` and `setDefaultRWConcern` answer 51300 "'<name>' is
  not supported on standalone nodes.", after their fields are parsed.

Fixes RM-551 (tests/backlog/test_rm_standalone_write_concern.py::test_a_standalone_implies_w_1)
