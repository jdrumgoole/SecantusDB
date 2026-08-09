### Oversized transactions are rejected before they can stall the engine

A multi-document transaction's statements all join a single WiredTiger
transaction, whose written content stays unevictable from the storage cache
until commit. A client that pushed enough data through one transaction
could therefore pin the cache past its dirty threshold and livelock the
engine — the same stall class the chunked-insert fix closed for plain batch
writes, where chunking cannot apply.

The Python server now enforces the guard real MongoDB has for this exact
condition: a transaction whose buffered write volume exceeds a budget
derived from the cache size (about 15%, mirroring mongod's threshold) fails
with `TransactionTooLargeForCache` (code 313). The error carries no
`TransientTransactionError` label — retrying the same oversized transaction
would hit the same wall — and, as with any failed in-transaction statement,
the transaction is aborted server-side. Transactions under the budget, and
plain writes of any size, are unaffected.

#### Added

- `secantus.storage`: `TransactionTooLargeError` + a per-transaction
  dirty-bytes budget (~15% of `cache_size`) enforced in the oplog-buffering
  path; surfaced by the command layer as mongod's
  `TransactionTooLargeForCache` (313, unlabeled). Pinned at both the
  storage and wire levels against a deliberately small cache.
