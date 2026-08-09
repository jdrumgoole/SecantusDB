### updateMany and deleteMany commit in bounded chunks on the Rust server

The last unbounded-transaction surface on the Rust server is closed:
updating or deleting every document a broad filter matches used to run as a
single WiredTiger transaction, whose unevictable dirty content grows with
the matched set — the same storage-livelock class the chunked batch inserts
and the transaction dirty budget already closed. Multi-document updates and
deletes now commit in bounded chunks (at most 1,000 documents or 4MB per
statement transaction), each chunk re-reading its documents inside its own
transaction so concurrent transaction commits are never overwritten from a
stale read, and each document is transformed exactly once even across
conflict retries.

Real MongoDB's updateMany and deleteMany are per-document write units and
documented as non-atomic, so the chunk boundaries match its semantics —
single-document writes, upserts, and writes inside multi-document
transactions are unchanged.

#### Fixed

- `secantus-storage`: `update_matching` (multi) and `delete_matching`
  (unbounded) run chunked statement transactions instead of one unbounded
  transaction. Pinned by `multiwrite_chunk.rs`: a 35,000-document rewrite
  and deleteMany against a deliberately small 128M cache, exactly-once
  `$inc` across chunk boundaries, and unchanged bounded paths.
