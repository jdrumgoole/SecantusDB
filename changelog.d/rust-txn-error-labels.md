### The Rust server's transaction errors carried the wrong label, and sometimes the wrong name

A driver decides what to do after a transaction fails by reading the error's
`codeName` and `errorLabels`. The Rust server got both wrong in ways that left
a driver's retry machinery either idle or running when it should not, and the
drivers' own specification suite said so in ten tests.

Everything below was measured against a single-node **replica set** mongod
8.2.11 over a raw OP_MSG socket. Both qualifiers earned their place: transactions
need a replica set, so the standalone used for most probing here cannot answer
these at all; and a driver in the path is not safe to measure through, because
pymongo retries `commitTransaction` itself and converts the NotPrimary family
into a client-side exception whose reply is never read. Two earlier passes did
exactly that and produced two confidently wrong columns.

#### Fixed

- **A `failCommand`-injected transaction error carried no label at all.** The
  failpoint short-circuit returns before the handler runs and before the
  transaction is resolved, so it never reached the labelling code that has been
  there all along. Every test in the drivers' transaction error-label suite
  injects its error that way, so every one of them saw `errorLabels: []`.
- **The label depends on WHICH COMMAND failed, which no server here modelled.**
  A statement inside a transaction gets `TransientTransactionError`; a
  `commitTransaction` or `abortTransaction` splits the same set in two, giving
  `RetryableWriteError` to the thirteen codes that are about reaching the node
  and keeping `TransientTransactionError` for the five that are about the
  transaction. Telling a driver the wrong one of those asks it to replay an
  entire transaction where mongod asks it to retry just the commit.
- **Six codes had no name and fell through to `Location<code>`** — 24, 112, 246,
  251, 267 and 11601. This is what made the spec's `commitTransaction fails
  after Interrupted` assert `Interrupted` and receive `Location11601`: the
  behaviour was right and the name was not.
- **An explicit `errorLabels: []` was indistinguishable from the key being
  absent**, because both parsed to an empty list. mongod treats a supplied list
  as authoritative and adds nothing to it, so an explicit `[]` means "no labels"
  rather than "you decide". Found by this change breaking two previously-passing
  spec tests, which is the argument for running the suite rather than the
  targeted tests.
- **Code 100 was `CannotSatisfyWriteConcern` on both servers**, a name mongod
  uses in neither context. A `w: 5` write against a single-node replica set and
  a `failCommand` injecting 100 both answer `UnsatisfiableWriteConcern`. The
  Python server's message for it also named the requested `w`, where mongod's is
  the bare `Not enough data-bearing nodes`. `tests/test_crud.py` asserted the old
  name, so the test was pinning our bug rather than mongod's behaviour.
- **Both servers were missing 134 and 262 from the transient set, together.**
  The Rust comment cited `commands.py::_TRANSIENT_TXN_CODES` as its authority,
  which is precisely how two engines stay in perfect agreement while both differ
  from the server they imitate. Neither the parity suite nor an engine-vs-engine
  sweep can see that; only the reference server can.

#### Added

- `failpoints::COMMIT_RETRYABLE_WRITE_CODES`, the measured commit/abort split,
  with unit tests that encode the mongod table as data — so a future divergence
  fails a fast Rust test instead of surfacing as an unlocalised red driver gauge.

#### Measured

- Rust server versus mongod 8.2.11: **0 divergent of 63** across `codeName` and
  `errorLabels` for 21 codes on `insert` / `commitTransaction` /
  `abortTransaction`, plus the explicit-`errorLabels` rule.
- `test_transactions_unified` against the standalone `secantusd-rs`: **10
  failures to 3**, and the three survivors are the secondary-read cases already
  recorded as an explicit non-goal — there is no secondary on a single node.
