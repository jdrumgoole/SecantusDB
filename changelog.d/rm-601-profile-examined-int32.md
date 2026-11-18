### A profile entry's examined counts are int32, as on mongod

The Rust MongoDB server's `system.profile` entries (and slow-query lines)
now write `keysExamined` and `docsExamined` as mongod 8.2.11 does: int32
when the count fits, int64 beyond.

#### Fixed

- `keysExamined` / `docsExamined` in a profile entry were int64.
  Fixes RM-601 (tests/backlog/test_rm_found_rm390.py::test_a_profile_entry_counts_what_it_examined_as_int32)
