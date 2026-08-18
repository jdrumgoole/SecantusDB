### statement_timeout is enforced

The `statement_timeout` GUC is now enforced, not just accepted: a statement
that runs longer than the configured timeout is cancelled with
`57014 canceling statement due to statement timeout`. The timeout is a
per-statement / per-message-batch deadline on the session that `pg_sleep`
(and other cancellation points) check, so a runaway query — for example
`SELECT pg_sleep(5)` under `SET statement_timeout='1s'` — returns control to
the client at the deadline instead of blocking. A bare numeric value is
milliseconds (PG's default unit); `s` / `min` / `h` / `ms` suffixes are
honoured; `0` disables it.

#### Added

- `session.py` / `functions.py` / `pgserver.py` / `pgextended.py`:
  `statement_timeout` enforcement — the deadline is armed at the first
  statement of a query / extended-protocol batch and checked in `pg_sleep`.

#### Known limitation

- A portal paged with `Execute MaxRows:N` is materialised eagerly, so
  `statement_timeout` cancels the whole materialisation rather than emitting
  some rows and then timing out on a later page (that incremental behaviour
  would need lazy per-row portal evaluation). Simple queries and single
  statements time out correctly.
