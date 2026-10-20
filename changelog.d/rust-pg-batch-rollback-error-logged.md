### Rust PostgreSQL server: a failed rollback after a failed batch is logged

When a statement in a simple-query batch failed, the server rolled back the
batch's implicit transaction and discarded any error from that rollback, so
a rollback failure left no trace anywhere. The client still gets the
statement's own error, as before; the rollback's error now goes to the
server's log, the way the retried-statement path already reported its own.

#### Fixed

- `secantus-pg`: `run_batch` logs a failed `rollback_implicit` instead of
  dropping it with `let _ =` (part of RP-049).
