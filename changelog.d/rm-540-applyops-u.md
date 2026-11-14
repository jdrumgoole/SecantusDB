### Rust MongoDB server: `applyOps` `u` entries answer as mongod does

`applyOps` now leaves a `u` replacement of a missing document unapplied when
the entry has no `b: true`, and refuses documents past 16 MB with mongod
8.2.11's codes.

#### Fixed

- A `u` entry replacing a document that does not exist, without `b: true`,
  is `false` in `results` (the command fails 8); it was inserted.
- A `u` upsert of a document past 16 MB is 17420 "Document to upsert is
  larger than 16777216" (was 10334), and an `i` entry replacing an existing
  document with one past 16 MB is 17419 (was 10334).

Fixes RM-540 (tests/backlog/test_rm_219_diag_query_settings.py::test_apply_ops_update_entries)
