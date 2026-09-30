### The Rust PostgreSQL server can be started and stopped inside an async test

`secantus_pgserver::bind` and the handle it returns could only be used from a
plain thread. Called inside a tokio runtime -- a `#[tokio::test]`, which is how
a Rust program writes a database test -- `bind` panicked before returning
("Cannot start a runtime from within a runtime"), and so did stopping or
dropping the handle ("Cannot drop a runtime in a context where blocking is not
allowed"). The Python embedding never hit either, because it calls from a plain
thread.

#### Fixed

- `bind` no longer blocks on the server's runtime: the listener is bound with
  the standard library and then handed to the runtime, which works from any
  context.
- `stop()` and `Drop` run their blocking half -- the connection drain, the
  runtime shutdown and the store's close-checkpoint -- on a thread of their own
  when called inside a runtime, and wait for it, so they still return only once
  the store is closed.
- New tests start, write, drop, reopen and read back inside both a
  current-thread and a multi-thread `#[tokio::test]`.
