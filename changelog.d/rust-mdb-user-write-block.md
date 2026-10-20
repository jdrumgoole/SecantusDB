### Rust MongoDB server: `setUserWriteBlockMode`

#### Added

- `setUserWriteBlockMode` blocks user writes as mongod 8.2.11 does. It
  used to be 59 CommandNotFound. Any write to a database other than
  `admin`, `config` and `local` is refused with 371 UserWritesBlocked at
  the point it would happen, so a command that changes nothing still
  succeeds. That covers no-match updates and deletes, existing indexes,
  and dropping something missing.
- Each refusal has mongod's shape: one write error per inserted document,
  updates and deletes under the update executor's wrapper, `$out` /
  `$merge` under the aggregate executor's, and a new index under the
  index build's (60 cases against mongod, none different).
- The block is kept in `config.user_writes_critical_sections`, so it
  survives a restart. It is lifted only with the reason it was set with,
  and is refused on a standalone. A user holding `bypassWriteBlockingMode`
  (`root`, `restore`) writes past it; with authorization off, nobody
  does.

#### Fixed

- Dropping a database that does not exist no longer writes a
  `dropDatabase` oplog entry, so a change stream no longer reports a drop
  that never happened. mongod writes none.

RM-219 part (`tests/backlog/test_rm_219_write_block.py`).
