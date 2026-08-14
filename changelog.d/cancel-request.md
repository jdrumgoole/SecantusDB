### Query cancellation: the wire CancelRequest is honoured

The PostgreSQL cancel sub-protocol — a client opens a fresh connection
and sends the (pid, secret) pair from BackendKeyData to cancel the query
running on its main connection — was parsed and silently dropped, and
`pg_sleep` ran as an uninterruptible `time.sleep`. Drivers lean on this
machinery for statement timeouts and context cancellation: pgx sends a
CancelRequest whenever a context is cancelled mid-query.

CancelRequest now fires the target session's cancel event (after
verifying the secret), and cancellation points observe it and raise PG's
`57014 canceling statement due to user request` while the connection
stays fully usable — cancel is not terminate. `pg_sleep` is such a point
in every context (FROM-less and per-row), `pg_cancel_backend` now
cancels instead of closing the target's connection (matching real PG;
`pg_terminate_backend` still closes), and a cancel that lands while the
session is idle is discarded, like real PG. In support of pgx's
liveness-poll shape, `pg_stat_activity` now reports an
extended-protocol statement's original text with `$1` placeholders
intact — the bound render inlined parameter values, which both leaked
them and made a `query like $1` poll match its own row.

#### Fixed

- `sql/pgserver.py` / `sql/session.py`: CancelRequest verifies the
  BackendKeyData secret and fires the target session's cancel event;
  stale cancels are discarded at the next statement's start.
- `sql/functions.py` / `sql/scalar.py`: `pg_sleep` waits interruptibly
  and raises 57014 on cancel, in FROM-less and per-row contexts alike
  (per-row numeric arguments arrive as Decimal128 and are now coerced);
  `pg_cancel_backend` cancels the target's running query without closing
  its connection.
- `sql/pgextended.py`: `pg_stat_activity.query` shows the prepared
  statement's original text (placeholders intact), not the
  parameter-inlined render.
