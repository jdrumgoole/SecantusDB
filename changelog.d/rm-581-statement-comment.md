### Rust MongoDB server: an `update` or `delete` statement shows its command's `comment`

A statement's `Slow query` line and profile entry now show the statement
with its command's `comment` as the last field, as mongod 8.2.11 does, so a
log search for the comment finds the statement's line.

#### Fixed

- Fixes RM-581 (tests/backlog/test_rm_statement_line_comment.py::test_a_statement_line_carries_its_commands_comment)
