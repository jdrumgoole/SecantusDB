### An UPDATE moves its row's unique-key claim

An update that moved a row off a unique value left the storage's claim on
the old value, so no other row could take that value again: an INSERT of it
failed `XX000 ... E11000 duplicate key error` where PostgreSQL 15.19 inserts
it, and a ROLLBACK TO that put back two rows which had swapped a unique value
was refused. The fix is in the shared `secantus-storage` crate, which the Rust
MongoDB server links too.

#### Fixed

- Every update path now claims the new value's keys (with `overwrite=false`,
  so two writers moving onto one value still collide in WiredTiger) and
  releases the old ones the row owns.
- A claim whose owner no longer holds the value -- the leftovers stores
  written before this fix carry -- is taken over by the next row that wants
  the value, instead of being refused.
- Fixes RP-296 (`tests/backlog/test_rp018_savepoint_undo.py::test_an_updated_unique_value_can_be_used_again`,
  `tests/backlog/test_rp018_savepoint_undo.py::test_savepoint_scenario[a unique column's values swapped-undo-journal]`,
  `crates/secantus-storage/tests/indexes.rs::an_update_moves_its_unique_claim`).

#### Changed

- The `start-session` and `close-session` skills run on Sonnet (`model: sonnet`).

#### Added

- A `fix-one` skill (`/fix-one <backlog> [ID|rule]`) that takes one Rust
  backlog item from claim to merged main, and `scripts/fix_one.sh`, which
  runs it in a fresh Claude Code session (an empty context).
