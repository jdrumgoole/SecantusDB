### A profiled update or delete statement's entry counts its locks

The Rust MongoDB server now counts the locks mongod takes to write an
update or delete statement's profile entry: its command's line, and each
later statement's, show one `Metadata: {W: 1}` and one more global,
database and collection IX per profiled statement, as on mongod 8.2.11.

#### Fixed

- A profiled `update` or `delete` reported fewer locks than mongod on its
  command's line and later statements' lines.

Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_a_profiled_statement_counts_its_entry_in_its_commands_locks)
