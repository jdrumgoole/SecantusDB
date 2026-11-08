### A READ COMMITTED block no longer fails with 40001 when a write slips in under its move

A READ COMMITTED block that has written is moved onto a fresh WiredTiger
snapshot at a statement's start whenever anything committed since, guarding its
rows across the moment its old transaction lets them go. A writer that had
already been through that guard's check could still write one of those rows in
the gap: its write stood in WiredTiger, uncommitted, while the guard turned the
same statement away at its next check. The writer then waited for the mover
holding the row, the mover's replay collided with the writer's write for as
long as it waited, and the replay gave up after its hundred attempts with
`40001 could not serialize access due to concurrent update` -- an answer
PostgreSQL never gives a READ COMMITTED block, which waits for the other
transaction and re-evaluates (`concurrent_writers_under_the_read_committed_move`
failed once in CI this way). The same race could instead let the mover's replay
overwrite the writer's committed update.

PostgreSQL has no such window: `heap_update` checks and stamps the tuple under
the buffer's content lock. The move now closes it the same way.

#### Fixed

- `secantus-storage`: a move installs its guard and rolls back its old
  transaction holding the write lock of every collection the transaction has
  written (sorted, as rename and dropDatabase take theirs). A writer holds its
  collection's lock from noting a row, where it looks for a guard, to writing
  it, so no writer is ever between the two: one that looked first has finished
  its write -- which collides with the old transaction, still holding the row --
  before the rollback, and one that comes later sees the guard. The replay takes
  the locks one write at a time, as any statement does.
- A deterministic regression test forces the interleaving through a storage test
  hook for a lone autocommit writer, the first write of a block and a write after
  others in its block, each at READ COMMITTED and REPEATABLE READ
  (`crates/secantus-pg/tests/embedded.rs::a_write_cannot_slip_in_under_a_moving_transactions_guard`).
