### Garbage SQL fails at parse time, and multi-statement errors stream partial results

sqlglot is a permissive parser: it reads `bad` as a column reference and
`SYNTAX ERROR` as an aliased expression, so preparing or executing a
non-statement quietly "succeeded" where real PostgreSQL raises a syntax
error. A bare expression at the top level is now rejected at parse time
with PG's `42601 syntax error at or near "..."` across every entry point
— simple protocol, extended-protocol Parse, and pipelined Parse.

A multi-statement simple query (`select 1; select 1/0; select 2`) also
now matches PG's streaming shape: the completed statements' results are
delivered before the ErrorResponse, and the statements after the error
never run. Previously a mid-batch error discarded the already-computed
results, so the client saw only the error.

#### Fixed

- `sql/engine.py`: top-level bare expressions raise `42601`; the
  expression-shaped commands sqlglot mis-parses the same way (`CLOSE`,
  `DISCARD`, `DEALLOCATE`) are exempted and keep working.
- `sql/pgextended.py`: the extended protocol's Parse applies the same
  check, so Prepare and pipelined SendPrepare error at parse time like
  real PG.
- `sql/engine.py` / `sql/pgserver.py`: a mid-batch `SQLError` carries the
  completed statements' results, and the wire layer renders them before
  the ErrorResponse, like real PG.
