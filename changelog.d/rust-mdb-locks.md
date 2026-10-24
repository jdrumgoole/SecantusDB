### Rust MongoDB server: the slow-operation log's locks and lines, `$merge`'s retries, and the admin commands' locks, as mongod counts them

Three lock-counting differences closed against mongod 8.2.11, all measured on
2026-10-24 against a reference mongod with plan yielding off.

**The slow-operation log.** When mongod logs an operation as slower than
`slowms`, and the operation took the global lock at all, it takes the global
lock again in IS to read the storage statistics for the `Slow query` line.
`ping`, `hello` and `serverStatus` take no lock, so their lines cost
nothing. mongod also logs more than commands:

- each update and delete statement, on a line of its own (component
  `WRITE`), before the command's line;
- each write a `$merge` makes, since those are update statements;
- an index build over documents, which logs once on its build thread
  (component `STORAGE`, type `none`).

The Rust server logged the command alone and took no lock for it.

**`$merge` with `whenMatched: "fail"`.** On mongod, a batch that meets a
duplicate is retried, and the retries take write rounds:

- the whole batch is tried at once and rolled back (a round, not taken for a
  batch of one);
- each retry from the start of what is left commits up to the next
  duplicate (a round each);
- the run after the last duplicate takes one more.

This fits all 27 measured shapes. The Rust server took one round.

**The admin commands.** A sweep of the 115 commands both servers list, run in
valid forms, found most informational commands taking a write chain on the
Rust server, where mongod takes no lock at all.

#### Fixed

- `secantus-commands` (`lock_manager.rs`, `slowop.rs`, `profiler/record.rs`,
  `admin.rs`, `lib.rs`), `secantus-server`: the slow-operation log.
  - An operation that took the global lock (`took_global`) takes mongod's
    global IS when it is logged.
  - Update and delete statements, `$merge`'s writes and an index build over
    documents write their own `Slow query` lines, each with its lock.
- `secantus-commands` (`aggregate.rs`): `$merge`'s `whenMatched: "fail"`
  retries take mongod's rounds (`merge_fail_rounds`).
- `secantus-commands` (`lock_manager.rs`, `stats_admin.rs`): mongod's locks
  for these commands:
  - none for `buildInfo`, `connPoolStats`, `connectionStatus`,
    `dropConnections`, `features`, `getClusterParameter`, `getCmdLineOpts`,
    `getDefaultRWConcern`, `getLog`, `getParameter`, `hostInfo`,
    `listCommands`, `top` and `whatsmyuri`;
  - a global IS for `rolesInfo` and `usersInfo`;
  - the replication state transition lock first for `dataSize`, `filemd5`
    and the four plan-cache commands;
  - the explained command's read, or one write round, for `explain`;
  - the database in S for `dbHash`;
  - one more global IS per database whose size it reads for
    `listDatabases`.
- `secantus-commands` (`misc_admin.rs`): `filemd5` over a chunks collection
  that does not exist answers no chunks and the MD5 of nothing, as mongod
  does. It was refused with 291.
- Fixes RM-299 (`tests/backlog/test_rm_185_locks.py::test_a_slow_operation_takes_the_global_lock_to_log`,
  `::test_a_write_statement_logs_a_line_of_its_own`) and RM-303
  (`tests/backlog/test_rm_found_2026_10_20b.py::test_filemd5_of_no_chunks_collection`).
  RM-185 is trimmed; its regression tests now include
  `::test_lock_counts_of_merge_fail_with_duplicates` and
  `::test_lock_counts_of_admin_commands`.

#### Filed

- RM-302: `getLog` lines are text (`<date> I COMMAND Slow query {...}`)
  where mongod's are its structured JSON log.
- RM-185 now names what is left. A malformed command still takes its lock
  chain here, where mongod takes none before it parses. `validateDBMetadata`
  still takes the generic chain. The transaction commands remain, and so do
  the commands outside the sweep.
