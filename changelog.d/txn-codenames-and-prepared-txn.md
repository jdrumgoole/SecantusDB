### Transaction failures reported `Location11601` where mongod says `Interrupted`

A `failCommand` failpoint renders its injected error code through the server's
code-name table, and that table had none of the transaction or replication
failure codes in it. So every injected transaction error came back named
`Location<code>` — `Location11601` for `Interrupted`, `Location251` for
`NoSuchTransaction`, `Location112` for `WriteConflict`. Drivers assert on
`codeName`, so the specs' `commitTransaction fails after Interrupted` failed not
because the behaviour was wrong but because the name was.

Seventeen names now come from a probe rather than the error-codes list, read off
a single-node **replica-set** mongod 8.2.11 — transactions need one, so the
standalone used for most probing in this file could not answer it.

The same probe settled a question that looked like one item and was two. The
driver gauges failed on `Interrupted` (11601) and
`PreparedTransactionInProgress` (267) together, and the obvious move was to add
both to the transient-label set. mongod labels 267
`TransientTransactionError` and gives 11601 **no labels at all** — so adding
both would have turned one test green and the other red. 267 is in; 11601 stays
out, which is where it already was.

#### Fixed
- `commands.py`: 17 transaction / replication codes now render mongod's real
  `codeName` instead of `Location<code>` — `Interrupted`, `NoSuchTransaction`,
  `WriteConflict`, `PreparedTransactionInProgress`, `SnapshotUnavailable`,
  `LockTimeout`, `NotWritablePrimary`, `InterruptedAtShutdown`,
  `InterruptedDueToReplStateChange`, `ShutdownInProgress`, `PrimarySteppedDown`,
  `SocketException`, `NetworkTimeout`, `HostUnreachable`, `HostNotFound`,
  `NotPrimaryNoSecondaryOk`, `NotPrimaryOrSecondary`.
- `267 PreparedTransactionInProgress` added to `_TRANSIENT_TXN_CODES`, measured
  rather than assumed.
- `test_transactions_unified.py`: 5 failures -> 3, and the remaining three are
  the secondary-read cases already recorded as an explicit non-goal (single node,
  no secondary to read from). Both retry-semantics failures are closed.
