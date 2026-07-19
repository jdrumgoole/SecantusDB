### SQL server: idle_in_transaction_session_timeout

A connection left idle inside an open transaction block longer than the
`idle_in_transaction_session_timeout` GUC (milliseconds; 0 = disabled, the
default) is now terminated with a FATAL `25P03` — the connection's blocked
read for the next command is bounded by the timeout, and exceeding it aborts
the open transaction and closes the socket, exactly as Postgres does.
psycopg's `test_right_exception_on_session_timeout` (which sets the GUC,
sleeps, and expects `IdleInTransactionSessionTimeout`) passes.

#### Added

- `idle_in_transaction_session_timeout` GUC (default 0); the wire loop bounds
  the next-command read by it while a transaction is open and terminates on
  timeout.
