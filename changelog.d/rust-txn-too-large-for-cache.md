### The Rust server rejects oversized transactions too

The Rust server now enforces the same transaction dirty budget the Python
server gained: a multi-document transaction whose written volume exceeds a
cache-derived threshold (about 15% of the storage cache, mirroring real
MongoDB's `TransactionTooLargeForCache` guard) fails with code 313 before
its unevictable content can stall WiredTiger. The error carries no
transient label and the transaction aborts, matching mongod. With this,
the storage-engine livelock class is closed on both servers: batch inserts
commit in bounded chunks, and transactions are bounded by the cache budget.

#### Added

- `secantus-storage`: `StorageError::TransactionTooLargeForCache` + a
  per-transaction dirty budget (~15% of the configured `cache_size`,
  default 4G) enforced across `with_user_transaction` statements; mapped
  to mongod's 313 by the command seam. Pinned by
  `txn_budget.rs::transaction_dirty_budget_guard` against a deliberate
  128M cache.
