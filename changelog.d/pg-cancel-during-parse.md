### The Rust PostgreSQL server no longer loses a cancel that arrives before execution

A `CancelRequest` that reached `secantusd-pg` while a statement was still being
parsed, bound or described was silently dropped, and the statement ran to
completion. The server cleared its pending-cancel flag at the moment a
statement began executing, on the reasoning that a cancel received while idle
targets nothing. That reasoning is right for an idle backend and wrong for one
that has already started on the client's message.

Probed against PostgreSQL 15.19 with a statement that takes seconds to parse:
PostgreSQL answered `57014` for a cancel sent anywhere from 0 to 640 ms after
the statement, and this server ran the full sleep at every offset. This is how
psycopg's `test_generators.py::test_cancel` ran its `pg_sleep(180)` to the end
on the Windows gauge runner.

#### Fixed

- A cancel is now dropped only if it arrives before the first message of a
  cycle (the backend is idle). One that arrives later stays pending until the
  next cancellation point, whichever message is being processed.
- A statement nested inside another (a `DO` block's inner statements) no
  longer clears a cancel aimed at the statement that contains it.
- A cancel is consumed by the statement it interrupts, so a handler that
  catches `query_canceled` is not cancelled again by the same request.
