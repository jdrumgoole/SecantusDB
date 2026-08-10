### Advisory locks now actually exclude

`pg_advisory_lock` and friends used to be session-local bookkeeping that
always granted — two connections could both "hold" the same exclusive lock,
so leader-election and migration-fencing patterns (alembic's lock, cron
fencing) silently provided no mutual exclusion. The PG server now runs a
server-wide advisory-lock table shared by every connection: exclusive and
shared modes with PostgreSQL's grant rules, re-entrant holds, blocking
`pg_advisory_lock` waits with deadlock detection (`40P01 deadlock
detected`), truthful `pg_try_*` results, and release on unlock, at
transaction end for `xact` locks, and when a connection ends.

#### Added

- `secantus.sql.pgadvisory.AdvisoryLockHub`: the server-wide lock table,
  attached to every wire session; per-session state remains the `pg_locks`
  reflection layer. Pinned by cross-connection tests covering exclusion,
  blocking waits, shared/exclusive interaction, deadlock detection,
  transaction-end and connection-teardown release — including a wire-level
  two-connection psycopg test.
