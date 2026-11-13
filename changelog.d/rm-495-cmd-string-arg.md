### Rust MongoDB server: a `Slow query` line names a collection only for a command that takes one

A `Slow query` line read any command's string value as the collection it
ran on. So a `dataSize: "<db>.c"` was logged under `<db>.<db>.c`, a
`getLog: "startupWarnings"` under `<db>.startupWarnings`, and a
`ping: "c"` under `<db>.c`. mongod 8.2.11 names a collection only for the
commands whose operation takes it as its namespace. Every other command's
line names `<db>.$cmd`. A sweep of every command in mongod's
`listCommands` measured which commands those are.

#### Fixed

- A line names `<db>.<value>` only for the commands that take a
  collection, as mongod 8.2.11's does: `find`, `aggregate`, `count`,
  `distinct`, the writes, `findAndModify`, `mapReduce`, `create`, `drop`,
  `collMod`, `collStats`, `createIndexes`, `dropIndexes`, `listIndexes`,
  `killCursors`, the `planCache*` commands, `compact`, `reIndex` and
  `convertToCapped`. Every other command names `<db>.$cmd`: `dataSize`,
  `getLog`, `dbCheck`, `setIndexCommitQuorum`, `ping` and the rest.
- A `compact`, `reIndex` or `convertToCapped` that failed names
  `<db>.$cmd`, because mongod's takes the collection only once it runs.
  That includes a `compact` or `reIndex` refused on a replica-set member,
  a collection that does not exist, and a missing `size`.
- A `cloneCollectionAsCapped` that ran names the capped collection it
  made, not its source. One that failed names `<db>.$cmd`.
- Fixes RM-495 (tests/backlog/test_rm_drop_reports.py::test_a_command_s_string_argument_is_not_its_collection,
  tests/backlog/test_rm_slow_query_namespace.py::test_a_data_size_line_names_its_own_namespace;
  crate test `slowop::tests::a_string_value_names_a_collection_only_for_a_command_that_takes_one`).
- Filed while working it: RM-595 (a `create` of an existing collection and a
  `findAndModify` refused as it parses still name the collection), RM-596
  (a `filemd5`, a `convertToCapped` and a `rolesInfo` of a role name what
  they work through on mongod) and RM-597 (`deleteIndexes` is refused as
  unknown).
