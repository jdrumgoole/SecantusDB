### A failed statement now aborts its transaction, whatever raised it

Postgres aborts a transaction block on any error: every later statement fails
until the block is rolled back. That held for errors raised while running a
statement, but not for errors the protocol layer raised on its own — asking for
a prepared statement or portal that no longer exists, or a parameter that could
not be decoded. Those left the block looking healthy, so work that a client
believed had been discarded went on to commit.

Rolling back, including to a savepoint, still recovers the block, and statements
outside a transaction are unaffected.

`DEALLOCATE ALL` also now reports the command tag Postgres reports —
`DEALLOCATE ALL` rather than a bare `DEALLOCATE`. Drivers watch for that exact
tag to learn their server-side statement cache has been discarded and to
re-prepare; without it they kept using names the server had already dropped.

The two go together. Aborting the transaction on its own made a JDBC driver's
recovery worse, not better: the block now died where the driver expected to
carry on, because it still had no idea its cache was stale.

#### Fixed

- An error raised by the extended query protocol aborts the open transaction.
- `DEALLOCATE ALL` reports the `DEALLOCATE ALL` command tag.
