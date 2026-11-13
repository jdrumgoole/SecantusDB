### A command refused as it parses never runs: no log line, no first command

mongod parses a command before it runs it, and a command its parse refuses
-- an unknown or mistyped field, a missing required one, a collection name
that is not a string, an unknown pipeline stage, an `lsid` that is not a
session id -- never became an operation: mongod 8.2.11 writes no `Slow
query` line and no profile entry for it, does not count it as a
connection's first command, and does not abort the transaction it was a
statement of. The Rust MongoDB server's handlers parsed their fields inside
their run, so it logged every such command, counted it as the connection's
first, and an aggregate refused for an unknown stage aborted the
transaction around it. Each handler's parse now runs ahead of the command,
before its authorization and its admission, as mongod's does, and the
`Slow query` line follows it.

#### Fixed

- `secantus-commands`: `find`, `insert`, `update`, `delete`, `aggregate`
  (its fields and the pipeline as mongod's `LiteParsedPipeline` reads it),
  `count`, `findAndModify`, `getMore`, `killCursors`, `drop`,
  `listCollections`, `listIndexes`, `createIndexes`, `renameCollection` and
  the command an `explain` explains are parsed in `parse_before_locks`,
  with every command's session fields; what mongod parses as the command
  runs (a filter, a sort, a stage's spec, an undefined `$$variable`) stays
  in the run, each placement measured against mongod 8.2.11.
  Fixes RM-386 (tests/backlog/test_rm_001_028.py::test_a_command_its_handler_fails_to_parse_is_not_the_first_command,
  tests/backlog/test_rm_slow_query_namespace.py::test_a_command_that_fails_to_parse_writes_no_line).
- `secantus-server`: the `Slow query` line is written only for a command
  that parsed (`CommandContext::parsed`), so a command the dispatcher
  refuses for its namespace, its generic arguments, its read concern or its
  `maxTimeMS`, and an unknown command, write none. A command refused
  once it has parsed -- for its session fields (`txnNumber` with no
  `lsid`), or on a database other than `admin` -- is logged, as mongod
  logs it.
- A transaction statement refused as it parses no longer aborts the
  transaction, and a `count` in a transaction with an unknown field is
  refused for the field (40415) before it is refused for the transaction.

#### Found

RM-602 (`create` and `dbStats` still parse in their run), RM-603 (nested
pipelines' stages are not lite-parsed), RM-604 (`$setWindowFields`,
`$densify`, `$fill` field types), RM-605 (Atlas stages' code and message),
RM-606 (`getMore` with an int cursor id), RM-607 (profile entries of
commands refused as they run), RM-608 (an `update`'s failed statement's
line).
