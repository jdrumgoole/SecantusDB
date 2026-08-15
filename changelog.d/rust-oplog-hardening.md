### Rust storage: bounded async-oplog buffers and poison-tolerant oplog locks

Two defence-in-depth fixes to the Rust storage engine's oplog path.

In async-oplog mode a multi-document transaction buffers every statement's
oplog entry — and, when pre-images are enabled, the pre-image bytes — on
the transaction handle for its whole lifetime. The entry bytes were already
charged to the transaction dirty budget (so an oversized transaction trips
`TransactionTooLargeForCache`), but the pre-image bytes were not, leaving
that half of the buffer able to grow unbounded within the 60-second
transaction-lifetime window. Pre-image bytes are now charged to the same
budget, so the whole buffer is bounded.

Two `self.oplog.lock().unwrap()` sites in the oplog-prune path used the
non-poison-tolerant form that the rest of the codebase had been swept clear
of — a panic while that lock was held would have permanently poisoned the
single mutex every write's oplog emission (and therefore change streams)
depends on. Both now use the poison-tolerant `unwrap_or_else(|e|
e.into_inner())`, and a source-scanning test guards `crates/secantus-storage`
and `crates/secantus-commands` against the pattern's reintroduction.

#### Fixed

- Async-oplog transactions now charge buffered pre-image bytes to the
  transaction dirty budget, so `pending_async` cannot grow the heap without
  bound (#750).
- The two oplog-prune mutex lock sites are poison-tolerant, matching every
  other lock on the server-wide oplog mutex; a held-lock panic no longer
  wedges oplog emission server-wide (#593). A `tests/` guard fails CI if the
  bare `.lock().unwrap()` pattern returns to either crate.
