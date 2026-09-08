### The Rust PostgreSQL server terminates and aborts like a real backend

The Rust `secantusd-pg` server now speaks the connection-lifecycle side of the
wire protocol the way a real PostgreSQL backend does: it can tell you its own
process ID, terminate a backend, and — crucially — surface the failure of a
statement inside a transaction so that the block is poisoned until it ends. A
`pymongo`-style permissive client never noticed the gaps here, but `psycopg`
does, and five of its `test_connection.py` lifecycle checks now pass against the
Rust server.

`pg_terminate_backend(pid)` behaves as it should on both sides of a connection.
Called on your own backend it ends the connection with a FATAL `57P01`
(`AdminShutdown`) and closes the socket, so the client learns the connection is
gone rather than believing it is still usable — a distinction that only shows up
over the simple query protocol, where PostgreSQL sends no `ReadyForQuery` after a
FATAL. Called on another backend it arms a flag that the victim notices at its
next statement and ends with the same `57P01`; terminating a PID that is not a
live backend returns `false`. `pg_backend_pid()` reports the PID pgwire assigned
during startup.

The other half is the aborted-transaction rule. A statement that fails inside a
transaction — whether it fails while the simple protocol splits it or while the
extended protocol describes it — now poisons the block, so every later statement
gets `25P02` (`InFailedSqlTransaction`) until `COMMIT`/`ROLLBACK`, and a `COMMIT`
of a failed block rolls back. Previously the failure went unrecorded and the
aborted block kept accepting commands.

#### Added

- `crates/secantus-pgplan` / `crates/secantus-pgserver`: `pg_backend_pid()` and
  `pg_terminate_backend(pid)` (self- and cross-connection), resolved from
  per-connection state through two new `ConstCol` variants and a process-wide
  backend registry.

#### Fixed

- `crates/secantus-pgserver`: an error inside a transaction now poisons the
  block from every path that can raise it — the simple protocol's statement
  split and the extended protocol's `Describe`, not only `Execute` — so the next
  statement correctly gets `25P02`.
- `crates/secantus-pgserver`: a FATAL error over the simple query protocol now
  closes the socket without a trailing `ReadyForQuery`, so the client sees the
  connection break instead of thinking it is still open.
