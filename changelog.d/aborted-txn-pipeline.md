### Aborted-transaction pipeline semantics match PG

Two wire-protocol fundamentals pinned by CockroachDB's pgtest corpus
(the `aborted_txn` file). Inside an ABORTED explicit transaction, every
extended-protocol step — Parse, Bind, Describe, Execute — now fails with
`25P02` until the transaction ends, with PG's transaction-exit carve-out
(`COMMIT` / `ROLLBACK` statements still parse and execute, or a client
could never leave the aborted block). Previously a Parse in an aborted
transaction quietly succeeded.

And an errored extended-protocol pipeline now discards **everything**
until Sync — including interleaved simple Query messages, matching PG's
`ignore_till_sync`. Previously a `Query` slipped through the discard and
executed (answering `25P02` and an extra ReadyForQuery the client never
expected).

#### Fixed

- `sql/pgextended.py`: 25P02 for extended steps in an aborted explicit
  transaction, with the COMMIT/ROLLBACK exemption.
- `sql/pgserver.py`: `ignore_till_sync` covers interleaved simple Query
  (and Fastpath) messages in an errored pipeline.
