### Rust PostgreSQL server: one committed DDL block no longer slows the connection for good

After a transaction block that changed the catalog or a role was committed
(`BEGIN; CREATE TABLE ...; COMMIT;`), the connection kept treating every
later statement as though its transaction had uncommitted catalog changes.
Every later statement re-read the catalog instead of using the shared
copies, and every later COMMIT on it invalidated every other connection's
catalog cache. On a debug build with 30 tables, each later
`begin; select 1; commit` took 204 ms instead of 1 ms, and a second
connection ran 1.8 times slower. Answers were never wrong. The marks are
now cleared when the transaction ends, on every path that ends one.

#### Fixed

- `secantus-pg`: Fixes RP-051. An explicit `COMMIT` never cleared the
  transaction's catalog-changed mark, and neither an explicit
  `COMMIT`/`ROLLBACK` nor a failed extended-protocol group cleared its
  role-written mark. Both are now `txn_state::TxnMarks`, ended by every
  transaction-end path (part of RP-049). `TxnMarks::end` is pinned by
  unit tests in `txn_state.rs`. The slowdown has no regression test,
  because the catalog version is not visible from outside the crate.
