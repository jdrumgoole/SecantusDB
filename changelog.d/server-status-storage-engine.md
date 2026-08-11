### `serverStatus` tells drivers which storage engine it is

Real driver test suites branch on `serverStatus.storageEngine.name` before
they will even attempt a transaction. SecantusDB never reported the field,
so mongo-php-library's `skipIfTransactionsNotSupported` helper threw
`UnexpectedValueException: Could not determine server storage engine` and
took roughly twenty-seven transaction tests down with it — not because any
transaction misbehaved, but because the suite could not establish what it
was talking to. One absent sub-document read as dozens of independent
failures.

Both servers now report the engine, and the answer is the true one:
SecantusDB is WiredTiger-backed, the same engine mongod uses. The
`persistent` flag is wired to the actual store rather than hard-coded, so
an `:memory:` instance reports itself as non-persistent instead of
claiming durability it does not have.

#### Fixed

- `serverStatus` now carries the `storageEngine` sub-document (`name`,
  `supportsCommittedReads`, `supportsPendingDrops`,
  `supportsSnapshotReadConcern`, `readOnly`, `persistent`,
  `backupCursorOpen`) on both the Python and Rust servers. The
  mongo-php-library gauge goes from 42 failures to 4 over the same 3130
  tests.
