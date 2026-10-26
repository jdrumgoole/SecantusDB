### Rust PostgreSQL server: a savepoint costs what is written after it, not the size of the table

In PostgreSQL a savepoint opens a subtransaction. Taking one costs nothing,
and rolling back to it costs as much as the subtransaction wrote. The Rust
PostgreSQL server used to copy every table a statement was about to write
into the savepoint, whole, on the first write after it. `ROLLBACK TO` then
rewrote the table from that copy whenever it could not move the block onto
a fresh WiredTiger transaction, so a one-row change under a savepoint cost
as much as the table held, in time and memory. A savepoint is now an undo
journal of rows. The first write of each row after a savepoint records the
row as it was, or that it did not exist, and a `ROLLBACK TO` that cannot
move the block puts back only those rows. The first create or drop of a
table records whether the table existed.

Measured on a 1,000,000-row table (optimised build, macOS arm64, best of
five) with `SAVEPOINT s`, a one-row `INSERT` and `ROLLBACK TO s`, the round took
163 ms when the block could move and 1,197 ms when it could not. It now
takes 1.6 ms and 2.2 ms. The insert after the savepoint went from 148 ms to
0.7 ms, and a `ROLLBACK TO` that cannot move from 1,045 ms to 0.2 ms.

Two answers changed to PostgreSQL's along the way. Data written into an
existing large object after a savepoint (`lo_put`) is undone by
`ROLLBACK TO`, and so is a `CREATE SEQUENCE`. The savepoint used to copy
only the tables a statement names, so it missed both. A sequence's `nextval` is still kept, because
PostgreSQL never rolls one back.

#### Changed

- `secantus-storage` (`UndoJournal`, on the transaction's `Held` rows):
  each document write inside a user transaction records the row's prior
  bytes in the innermost open savepoint frame that has not got it yet.
  Collection creates and drops record whether the collection existed. A
  move's replay records nothing. With no frame open, nothing is recorded.
  The MongoDB server opens no frames, so its behaviour is unchanged.
- `secantus-pg` (`savepoint_op_inner`, `apply_undo`): SAVEPOINT opens a
  frame, RELEASE merges it into the enclosing one, and ROLLBACK TO takes
  the oldest record of each row among the savepoint and those inside it.
  `capture_for_savepoints` now captures only index definitions.
- `secantus-pg` (`rollback_to_by_move`): a move undoes every write after
  the savepoint except a sequence's advance, rather than only the writes
  to the tables the capture had copied.

#### Fixed

- `ROLLBACK TO SAVEPOINT` undoes `lo_put` and `CREATE SEQUENCE` made
  after the savepoint, as PostgreSQL 15.19 does. Fixes RP-018
  (`tests/backlog/test_rp018_savepoint_undo.py`: 25 savepoint scenarios,
  each run by a move and by the journal against 15.19's answers, and
  `test_savepoint_cost_is_the_rows_written_not_the_table`).
