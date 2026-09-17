### The Rust PostgreSQL server prepares transactions

Two-phase commit is how a transaction manager coordinates one commit across
several databases: each participant is asked to `PREPARE TRANSACTION 'gid'`,
which must make the work durable without making it visible, and only once
every participant has said yes does the manager `COMMIT PREPARED 'gid'` on
each of them — from whichever connection it happens to hold at the time,
possibly after the original one has gone, possibly after the server has been
restarted in between. The Rust PostgreSQL server now does all of that.
`PREPARE TRANSACTION` records the block's write set durably and ends the
block; the writes stay invisible to everyone; `COMMIT PREPARED` and
`ROLLBACK PREPARED` resolve the transaction from any connection, and a
prepared transaction found on disk at startup — data and DDL alike — is
listed in `pg_prepared_xacts` and replayed when it is committed.
`max_prepared_transactions` reports 100, so psycopg's two-phase-commit suite,
which had skipped itself against the server, now runs: 38 tests pass.

The error surface is PostgreSQL 16's, each case measured: a `PREPARE`
outside a block is a warning and a `ROLLBACK`, every failed `PREPARE` ends
the block, a duplicate identifier is `42710`, one of 200 bytes or more is
`22023`, a block that created a temporary table, opened a `WITH HOLD` cursor
or used `LISTEN` / `NOTIFY` cannot be prepared (`0A000`), and `COMMIT
PREPARED` inside a block is `25001`. `TRUNCATE` landed on the way, because
psycopg's fixture uses it: `TRUNCATE [TABLE] a, b [RESTART IDENTITY]
[CASCADE]`, with PostgreSQL's refusal of a foreign-key parent (detail and hint
included) and its per-table cascade notices.

#### Added

- `PREPARE TRANSACTION` / `COMMIT PREPARED` / `ROLLBACK PREPARED` and the
  `pg_prepared_xacts` view on the Rust PG server; `max_prepared_transactions`
  is 100. `secantus-storage` gains a `secantus_prepared_xacts` table,
  `prepare_user_transaction` / `commit_prepared` / `rollback_prepared` /
  `list_prepared_xacts`, and a replay of a recorded write set for a
  transaction prepared before a restart.
- `TRUNCATE [TABLE] name [, ...] [RESTART IDENTITY] [CASCADE]`.

#### Fixed

- `Storage::outside_user_transaction` (the oid counter a `CREATE TABLE` mints
  mid-block) let its inner autocommit statement drain the enclosing block's
  parked oplog seq ranges — and, in async oplog mode, clear its buffered
  entries — so the block's earlier writes were lost to the in-flight window
  and to the prepared write set. It now stashes and restores the thread's
  oplog bookkeeping around the bookkeeping call.
