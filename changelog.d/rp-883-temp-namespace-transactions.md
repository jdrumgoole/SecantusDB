### The temporary schema follows the transaction that made it

A session's temporary schema now lives and dies as PostgreSQL's
`myTempNamespace` does. Made by a transaction or savepoint that rolls back,
it is gone again: `pg_my_temp_schema()` answers 0 and `pg_temp` names no
schema. Made inside a transaction that commits, it stays even when the
temporary table that made it was dropped in the same transaction, so a call
to a missing `pg_temp` function is the missing function's 42883 rather than
a missing schema. A RELEASE hands it to the enclosing level.

#### Fixed

- Rust PG server: the temp namespace keeps the (sub)transaction level that
  made it (`myTempNamespaceSubID`) and is unmade when that level aborts.
  Fixes RP-883 (tests/backlog/test_rp_qualified_calls.py::test_temp_namespace_follows_transactions)
