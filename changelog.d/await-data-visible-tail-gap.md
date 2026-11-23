### A tailing `awaitData` wait is no longer woken by a write that has not committed

#### Fixed

- A filtered `awaitData` getMore woken by a write its filter rejects
  sometimes counted 8 execution admissions where mongod 8.2.11 counts 6
  (1 run in ~15 under load; RM-764). Cause: the oplog's visible tail, which
  a tailing wait compares against, was `start - 1` of the oldest write
  still in flight. A timestamp no entry holds (a new second restarts the
  counter; a commit stamp consumes one) leaves a gap below that start, so
  a wait that timed out while the write was in flight read the tail as
  advanced, rescanned for a document that was not there yet, and waited
  again: a third pair of admissions. The tail is now the last entry
  minted before the in-flight run, so only an entry that exists moves it.
- Fixes RM-764. Tests:
  `an_in_flight_write_does_not_move_the_visible_tail_across_a_timestamp_gap`
  (`crates/secantus-storage/src/lib.rs`, deterministic: a stamp leaves the
  gap, the wait must not wake) and RM-355's
  `test_an_await_data_get_more_woken_twice_takes_mongods_tickets`
  (`tests/backlog/test_rm_355_364.py`).
