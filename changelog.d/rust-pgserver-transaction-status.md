### A transaction you can see, and a failed one that says no

Every connection to the Rust PostgreSQL server reported itself as idle. Inside a
transaction, after a failed statement, in the middle of a block — always idle.
The status rides on the message that ends every statement rather than being
something a client selects, so nothing in a row comparison could ever see it,
and clients that steer on it were steering blind.

The half that was more than cosmetic: PostgreSQL aborts a transaction block at
the first error and refuses everything after it until the block ends. This
server carried on. A client that shrugged off a mid-transaction error went on
writing and committed work PostgreSQL would have discarded — a wrong answer,
not a missing feature. Statements after an error in a block now get `25P02`
until the block ends, and a `COMMIT` there is a rollback that says `ROLLBACK` in
its command tag, exactly as PostgreSQL's does.

Two smaller spellings came along: `START TRANSACTION` answers with its own
command tag rather than `BEGIN`'s, and `COMMIT AND CHAIN` / `ROLLBACK AND CHAIN`
open the next block immediately instead of leaving the connection idle — a
client that chained was previously left autocommitting its next statements one
at a time.

#### Added

- `AND CHAIN` on `COMMIT` and `ROLLBACK`.
- `START TRANSACTION`'s own command tag.

#### Fixed

- Every connection reported `IDLE` whatever the transaction state.
- Statements after an error inside a transaction were executed rather than
  refused with `25P02`, and a `COMMIT` of a failed block reported `COMMIT`.
