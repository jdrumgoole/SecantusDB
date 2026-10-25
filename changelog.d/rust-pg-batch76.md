### The Rust PostgreSQL server: duplicate keys wait, and big transactions fit

Two transactions inserting the same key now behave as in PostgreSQL. The
second waits for the first. If the first rolls back, the insert goes on. If
it commits, the insert fails 23505 under REPEATABLE READ, and with
serializable snapshot isolation's reason code under SERIALIZABLE. Before, it
waited and then failed with 40001 "concurrent update" either way.

A single transaction can also hold about twice as many rows: a million-row
INSERT now commits.

#### Fixed

- `secantus-pg`: a duplicate-key insert under REPEATABLE READ or
  SERIALIZABLE (RP-255, all but an INSERT inside a function a constant
  SELECT calls, which stays open:
  `tests/backlog/test_rp_isolation.py::test_unique_insert_waits`).
  - The page-model checkpoints no longer count as commits that a waiting
    transaction's snapshot misses: they ran during the wait and turned a
    rolled-back holder into a 40001. They are written outside any
    transaction and read only at startup.
  - A key the holder committed is the 23505 PostgreSQL raises whatever the
    snapshot. Under SERIALIZABLE, SSI's conflict-in on the key's leaf
    comes first, as in `_bt_check_unique`.
  - A unique index's insert-time SSI check now runs once uniqueness is
    decided, so a waiting insert no longer flags a conflict early.
  - A SELECT, CALL or DO whose functions write now waits for a row as their
    INSERT would.
- `secantus-pg`: SSI's own 40001 is no longer taken for a write conflict, so
  it can never be waited out and run again.
- `secantus-pg`: WiredTiger's eviction triggers are raised to 50% for the PG
  server, so one transaction holds about 1,000,000 small rows at the 4G
  cache instead of 500,000. Fixes RP-256
  (`tests/backlog/test_rp_isolation.py::test_million_row_transaction`). The
  remaining limit is a scope decision (Joe, 2026-10-25).
- `secantus-storage` (shared with the MongoDB server):
  `without_commit_count` runs writes that no snapshot reads without counting
  them as commits. Only the PG server's checkpointer uses it.
