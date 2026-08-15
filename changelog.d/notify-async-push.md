### Notifications reach idle connections without waiting for a query

LISTEN/NOTIFY delivery was piggybacked on the query cycle: a queued
notification was written to the listener's socket only when that
connection next issued a command. A client that just blocks reading the
socket — pgx's `WaitForNotification`, psycopg's `notifies()` — waited
forever, because real PostgreSQL pushes notifications to idle
connections asynchronously.

Listening sessions now wait for their next command in short slices and
flush queued notifications between them, from the connection's own
thread so socket writes stay serialized. Sessions with no LISTENs — the
overwhelming default — keep the pure blocking read, so there is no
busy-wake cost for ordinary connections, and the
idle-in-transaction-session-timeout deadline is preserved across the
poll slices.

#### Fixed

- `sql/pgserver.py`: the idle read loop pushes queued notifications to
  listening sessions (~250 ms delivery latency) instead of holding them
  until the next query cycle.
- `sql/pgnotify.py`: `NotifyHub.is_listening` — the poll applies only to
  sessions with at least one active LISTEN.
