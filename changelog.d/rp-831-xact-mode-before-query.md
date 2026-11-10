### Rust PG server: a transaction's modes are fixed once it has run a query

PostgreSQL fixes a transaction's isolation level, read-write mode and
deferrable mode as soon as one of its statements takes a snapshot. After
that, `SET TRANSACTION ISOLATION LEVEL SERIALIZABLE` is refused with 25001.
The Rust PostgreSQL server accepted the change and moved the setting, even
though the transaction's snapshot had already been taken under the old
mode. A client that thought it had switched to SERIALIZABLE part-way through
a block was running under weaker guarantees than it asked for, and nothing
told it so.

The server now keeps PostgreSQL's `FirstSnapshotSet` for each transaction
and runs the three settings' check hooks from `variable.c`, in their order,
wherever a mode is set. All of the answers were measured on PostgreSQL
15.19.

#### Fixed

- `crates/secantus-pg`: once a transaction has run a statement that takes
  a snapshot (anything except transaction control, LOCK, SET, RESET, SHOW,
  SET CONSTRAINTS, FETCH / MOVE, LISTEN / NOTIFY / UNLISTEN and
  CHECKPOINT), setting a different isolation level is 25001 `SET
  TRANSACTION ISOLATION LEVEL must be called before any query`. Moving from
  read-only to read-write is 25001 `transaction read-write mode must be set
  before any query`, and any `[NOT] DEFERRABLE` is 25001 `SET TRANSACTION
  [NOT] DEFERRABLE must be called before any query`. The check covers
  `SET TRANSACTION`, `SET` / `SET LOCAL transaction_*`, `set_config(...)`
  and a BEGIN's modes in an open block or a multi-statement query's
  implicit block. A SELECT takes its snapshot before its select list runs,
  so `select set_config('transaction_isolation', 'serializable', false)`
  is refused even on its own. A ROLLBACK TO SAVEPOINT does not give the
  snapshot back. `SET ... TO DEFAULT` and `RESET` install the reset value
  without the check, as PostgreSQL's do.
- Inside a savepoint, a different isolation level is 25001 `... must not
  be called in a subtransaction`. Read-write in a read-only transaction is
  25001 `cannot set transaction read-write mode inside a read-only
  transaction`, and a deferrable setting is 25001 `... cannot be called
  within a subtransaction`. Before this change all three were accepted.
- A PL/pgSQL `COMMIT` starts a transaction with no snapshot, so `commit;
  set transaction isolation level repeatable read;` in a DO block still
  works, as it does in PostgreSQL's `plpgsql_transaction` test.

Fixes RP-831 (tests/backlog/test_rp_found_rp829.py::test_a_transaction_mode_is_set_before_any_query,
tests/backlog/test_rp_found_rp829.py::test_the_transaction_mode_check_hooks,
tests/backlog/test_rp_found_rp829.py::test_an_extended_protocol_block_fixes_its_mode_at_its_first_query)
