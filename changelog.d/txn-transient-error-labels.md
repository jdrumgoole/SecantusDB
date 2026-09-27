### A transaction error injected by a failpoint carried no `errorLabels`

A driver decides whether to retry a whole transaction by reading the
`TransientTransactionError` label off the error. SecantusDB attached that label
to errors it raised itself, but not to errors produced by a `failCommand`
failpoint — and since every test in the drivers' transaction error-label suite
injects its error that way, the entire suite saw `errorLabels: []`. A driver's
retry loop, exercised exactly as the specification intends, silently did not
engage.

The labelling logic already existed and was already correct. `failCommand`'s
short-circuit simply returned before reaching it: it answers before the handler
runs, and before the transaction is even resolved. So this is six lines routing
one path through machinery the other path had been using all along, not new
semantics — the third time in this file that a gap turned out to be an existing
mechanism one caller never reached.

#### Fixed
- `commands.py`: a failpoint-injected error whose code is in
  `_TRANSIENT_TXN_CODES` now carries `TransientTransactionError` when the
  statement is part of a transaction, matching what a naturally-occurring error
  of the same code already did. Detected via `autocommit: false` — the signal
  mongod itself uses, present on every statement of a transaction including
  commit and abort — so the failpoint check does not have to move below the
  transaction resolution and reorder failpoint-versus-transaction error
  precedence, which no probe currently justifies.
- Takes `test_transactions_unified.py` from 9 failures to 5, including all three
  of `TestUnifiedErrorLabels` (`NoSuchTransaction`, `NoSuchTransaction` on
  commit, `WriteConflict`).

#### Changed
- Reading from a secondary is now an explicit non-goal rather than a gap.
  SecantusDB is single-node, so there is no secondary to read from and anything
  whose behaviour is defined by one is out of scope. Three driver-gauge tests
  fail permanently on this and are accepted with the reason written down in
  `tasks/backlog.md` §4, rather than deselected to tidy the number.

#### Deferred
- Two failures remain, and the two error codes want OPPOSITE treatment:
  `Interrupted` (11601) where the test expects the commit to fail, and
  `PreparedTransactionInProgress` (267) where it expects the transaction to be
  retried. Adding both to the transient set would make one green and the other
  red, so `tasks/backlog.md` §5 says to probe a single-node replica-set mongod —
  transactions need one, so a standalone cannot answer it — before moving a label
  set on a guess.
