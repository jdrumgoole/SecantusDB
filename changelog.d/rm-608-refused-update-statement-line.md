### Rust MongoDB server: an update statement refused for its modifiers writes its own `Slow query` line

An `update` whose statement names an unknown modifier (`{$bogus: {a: 1}}`)
now logs that statement on a `WRITE` `Slow query` line before the
command's, as mongod 8.2 does: `ok: 0`, the parse error, no plan and no
counts.

#### Fixed

- A refused update statement wrote no line of its own, only the command's.
  Fixes RM-608 (tests/backlog/test_rm_found_rm386.py::test_an_update_with_a_failed_statement_writes_two_lines)
