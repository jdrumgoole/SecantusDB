### Rust PG server: no parse starts once a cancel is pending

A cancel that lands while a large statement is parsed abandons the parse
thread and answers 57014. But a pre-pass that swallows that error (the
privilege and lock passes) went on to parse the statement again, on a new
thread, after the cancel. On a small CI runner that parse, and the freeing
of its 200,000-item tree, took the CPU the cancel's answer was waiting for:
RP-002's regression test answered 0.37 s after the cancel, against
PostgreSQL 15.19's 0.015 s, and the cancel trace showed the second parse
thread starting 19 ms after the cancel.

#### Fixed

- `cancel::interruptible` answers a cancel already pending before it starts
  any work, as PostgreSQL's next `CHECK_FOR_INTERRUPTS` would, so no parse
  or tree copy begins after a cancel. RP-571 stays open as a watch item:
  parse threads abandoned before the cancel still run to the end, since
  libpg_query cannot be stopped mid-parse.

Part of RP-571 (crates/secantus-pgplan/src/cancel.rs::tests::a_pending_cancel_starts_no_work).
