### A cancel during a named statement's Parse is no longer lost

The Rust PostgreSQL server lost a cancel that arrived near the end of a
named statement's Parse, which is how psycopg's `prepare=True` sends one.
After analysing the statement, the Parse built its `pg_prepared_statements`
row: a second scan and describe of the statement, with no cancellation
point. A cancel that landed then was still pending when the Parse answered.
The Sync closed the cycle, the Bind's first byte cleared the cancel as
stale, and the statement ran to its end. PostgreSQL checks for interrupts
throughout `exec_parse_message`, so there the same cancel is the Parse's
57014 and nothing is stored.

This was the cancel RP-215's regression test lost nine times under load, in
CI and locally. The test's 200,000-item statement takes about 2 s to parse
on a debug build, close to the cancel it sends at 2.5 s. A sweep of cancel
offsets reproduced it on an idle machine: every cancel sent 0.02 to 0.08 s
before the Parse answered was lost.

#### Fixed

- `secantus-pg`: a named Parse builds its `pg_prepared_statements` row under
  the cancel hook and takes a cancel before it stores the statement, so a
  cancel then is the Parse's 57014.

Fixes RP-303 (tests/backlog/test_rp_named_parse_cancel.py::test_a_cancel_while_a_named_parse_records_its_statement_is_the_parses_57014)
