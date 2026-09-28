### A terminated connection now hears why

`pg_terminate_backend`, an idle-session timeout and an idle-in-transaction
timeout all end a connection with a FATAL error, and the client is supposed to
see that error on its next round trip. It often saw nothing of the kind: on
Linux the `57P01` surfaced only afterwards, buried in psycopg's rollback
warning, and on Windows it vanished entirely behind a bare
`Software caused connection abort (10053)` with no SQLSTATE at all.

The error was being sent correctly and then destroyed in transit. After the
FATAL the connection loop breaks, `process_socket` returns, and the socket is
dropped — closed outright. The client's next statement is a *write* to a closed
socket, which draws a TCP RST, and an RST discards whatever the client has not
yet read. The FATAL was sitting in exactly that buffer.

#### Fixed

- The server now ends a connection with a lingering close
  (`crates/vendor/pgwire/src/tokio/server.rs`): it shuts the write half down
  first, sending a FIN so the client reads the error and then a clean EOF, and
  drains reads afterwards so the socket stays half-open long enough for the
  client's in-flight bytes to be consumed rather than reset. Applied to both
  `process_socket` and `process_socket_unix`.

  The drain budget is a ceiling rather than a delay — it ends at the client's
  EOF, so an ordinary `Terminate`-and-hang-up costs nothing measurable. On the
  Rust PG server's slice tests the change is a straight improvement in both
  directions: **229.8s with three failures becomes 95.1s with none**, because
  the connection aborts had been burning time in retries.

- The same tests were also killing the server rather than stopping it on
  Windows. `proc.terminate()` is SIGTERM on POSIX but `TerminateProcess`
  there — an immediate kill that runs no handler, so WiredTiger never closed
  and anything not yet checkpointed was lost, which is why the cross-language
  hand-off tests opened an empty store. The binary already installed a console
  control handler; it was never sent a signal it could catch.
