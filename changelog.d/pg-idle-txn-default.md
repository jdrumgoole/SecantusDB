### The PG server now ships a default idle-in-transaction timeout

A PostgreSQL client that opens a transaction and then goes quiet — a failed
test that never rolls back, a leaked pooled connection — used to pin the
storage engine's oldest snapshot indefinitely. WiredTiger then had to keep
every subsequent write's history reachable, so each operation got slower in
proportion to total churn until a large statement (a 100k-row TRUNCATE in
pgjdbc's own suite) stalled in page reads and wedged the whole server. That
single mechanism was the root cause of the pgjdbc conformance lane's
two-hour hang.

`SecantusPGServer` now applies a server-config default of 120 seconds for
`idle_in_transaction_session_timeout` (PG ships 0/disabled, but PG's MVCC
degrades gracefully where WiredTiger's cache-bound history does not). The
GUC hierarchy is faithful: a session `SET` overrides the server default,
`SET … = 0` opts out entirely, `RESET` falls back to the server value, and
`SHOW` reports the effective setting. The `secantusd-py-pg` daemon grows a
matching `--idle-in-transaction-timeout` flag.

#### Added

- `Session.server_gucs` — a postgresql.conf-tier defaults layer between
  session `SET` overrides and the built-in GUC defaults, honoured by
  `get_setting`, `SHOW`, `SHOW ALL`, and `RESET`.
- `SecantusPGServer(idle_in_transaction_timeout_s=…)` constructor knob and
  the `--idle-in-transaction-timeout` daemon flag (default 120s, 0 disables).

#### Fixed

- An abandoned open transaction on a live connection no longer degrades all
  later writes without bound (linear-with-churn slowdown, ending in a
  server-wide page-read stall). Idle-in-transaction sessions are terminated
  with PG's own FATAL 25P03 after the timeout, unpinning the snapshot.
