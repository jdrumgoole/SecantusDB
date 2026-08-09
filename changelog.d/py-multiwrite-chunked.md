### updateMany and deleteMany commit in bounded chunks on the Python server too

The Python server gains the same bounded multi-document write transactions
the Rust server just did: updating or deleting everything a broad filter
matches no longer runs as a single WiredTiger transaction whose unevictable
dirty content grows with the matched set. Chunks re-read their documents
inside their own transaction, every document is transformed exactly once
even across conflict retries, and single-document writes, upserts, and
writes inside multi-document transactions are unchanged. With this, the
storage-engine livelock class is closed on both servers across all three
surfaces: batch inserts, multi-document updates and deletes, and
multi-document transactions.

#### Fixed

- `secantus.storage`: `update_matching` (multi) and `delete_matching`
  (unbounded) run chunked statement transactions (≤1000 docs / ≤4MB each)
  instead of one unbounded transaction — mongod-faithful, since updateMany
  and deleteMany are per-document write units and documented non-atomic.
  Pinned by a 35,000-document rewrite + deleteMany against a deliberate
  128M cache, exactly-once `$inc` across chunk boundaries, and unchanged
  bounded paths.
