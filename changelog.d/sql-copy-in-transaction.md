### SQL: COPY runs inside the open transaction block

The COPY sub-protocol handler never entered the session's user transaction:
`COPY` after a same-block `CREATE TABLE` failed with `UndefinedTable`
(psycopg's standard fixture shape, cascading through ~190 of its COPY-backed
tests), `COPY TO STDOUT` couldn't see rows inserted earlier in the block —
and worst, `COPY FROM STDIN` rows were written *outside* the transaction, so
they survived a `ROLLBACK`. Plan resolution, the copy-in insert, and the
copy-out extract now all run under `use_user_transaction` when a block is
open, and a failed COPY marks the block aborted like Postgres does.

#### Fixed

- `pgserver.py`: `_handle_copy` / `_copy_in` / `_copy_out` wrap their engine
  calls in the session's open user transaction (no-op outside a block);
  a COPY error inside a block sets `txn_failed`. The three copy-heavy psycopg
  suites (test_copy / test_range / test_multirange) move 230 → 374 passing.
