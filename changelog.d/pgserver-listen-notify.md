### The Rust PostgreSQL server delivers notifications, terminates backends, and runs the whole psycopg suite

The psycopg gauge measured a third of psycopg's test suite against the Rust
PostgreSQL server: the synchronous modules, with the asynchronous twins,
notifications, pipelining, two-phase commit, concurrency and the libpq-level
tests left for "a later lane". This release runs all of them, and the server
was fixed until the widened gauge is clean.

The largest piece is `LISTEN` / `NOTIFY`. A notification is delivered the way
PostgreSQL delivers it: to every listening connection, the sender included,
before the statement's ReadyForQuery outside a transaction block, at `COMMIT`
inside one (queued, deduplicated, dropped on `ROLLBACK`), and unprompted to a
connection sitting idle. That idle path is what `pg_terminate_backend` needed
too — it used to set a flag the victim noticed on its next statement, so a
session asleep in `pg_sleep` or idle never went away. Both it and the new
`pg_cancel_backend` now reach the victim within milliseconds, whether it is
running a statement or waiting for one.

The rest is throughput and shape. One psycopg test timed out because every
statement re-read the type catalog from storage, re-looked-up its table and
re-parsed its SQL up to four times; a process-wide catalog cache and a parse
memo bring a trivial statement from 0.86 ms to under 0.2 ms and a 20,000-row
`executemany` from 15 s to 3.4 s. `CREATE TABLE AS`, functions in `FROM`,
aggregates over expressions, `pg_tables`, `now()` and its siblings, and the
extended protocol's Describe replies for undeclared parameters all landed
because psycopg's tests asked for them, each measured against PostgreSQL 16.

#### Added

- `LISTEN` / `UNLISTEN [*]` / `NOTIFY` / `pg_notify()` / `pg_listening_channels()`
  with asynchronous cross-connection delivery on the Rust PG server. The
  vendored pgwire gains an `idle_event` hook the connection loop races against
  the next frontend message, and a `before_ready_for_query` hook.
- `pg_cancel_backend(pid)`; `pg_terminate_backend(pid)` now ends an idle or
  sleeping session immediately, and an unknown pid answers `false` with
  PostgreSQL's `01000` warning.
- `CREATE [TEMP] TABLE [IF NOT EXISTS] t [(cols)] AS query [WITH [NO] DATA]`.
- A function call as the row source in `FROM` (`select 'ok' from pg_sleep(0.5)`,
  `select * from pg_listening_channels()`).
- Aggregates over expressions (`max(length(data))`, `sum(col1 * 2)`).
- The `pg_tables` catalog view.
- `now()` / `transaction_timestamp()` / `statement_timestamp()` /
  `clock_timestamp()` and `current_timestamp`, typed `timestamptz` and
  rendered with the session-zone offset.
- A process-wide catalog cache (type-catalog documents and table lookups,
  versioned by every catalog-changing statement and transaction control) and
  a memo of `pg_query` parse trees and parameter counts.
- The psycopg gauge now includes every `test_*_async.py` twin, `test_notify`,
  `test_pipeline`, `test_concurrency`, `test_tpc`, `test_xid`,
  `test_conninfo_attempts`, `test_waiting`, `test_module` and `tests/pq`, with
  a per-platform pytest marker filter (`MARKER_EXPR`) that excludes psycopg's
  `proxy` and `timing` markers on macOS as psycopg's own CI does. Three tests
  are deselected with their reasons recorded in `include_paths.py`: one reads
  a foreign libpq's `PGconn` and two connect to RFC 5737 unroutable addresses
  and depend on the host network timing out rather than answering at once.

#### Fixed

- Describe of a prepared statement reports each undeclared parameter's type
  from the statement instead of `unknown`, and integer arithmetic over
  parameters describes as the operands' type.
- `begin; declare cur ...` in one simple-query batch keeps the transaction
  and the cursor open; a wire Close of a portal closes the `DECLARE`d cursor
  of that name; a missing portal is `34000`, a missing statement `26000`.
- `now()::text` rendered the wall clock with no zone suffix; it now carries
  the session-zone offset like every other `timestamptz`.
- `current_timestamp` inside an expression (`current_timestamp::text`) was
  refused; it evaluates.
- Per-statement cost grew with every table the store had ever held: each
  statement re-decoded every row type from BSON for the planner and again per
  described column, so a used store ran `select 1` twice as slowly as a fresh
  one and `test_type_error_shadow` overran its 20 s budget late in the gauge.
  The type catalog is now shared by reference, the planner's tables are
  published once per thread per catalog version, and a described column decodes
  only the one composite it names.
- `password_encryption` reports `scram-sha-256`; `ALTER ROLE` of a role
  other than the session user is `42704`.
