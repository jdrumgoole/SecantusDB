### The Rust server's batch inserts are chunk-committed too

The Rust storage engine had the same latent hazard the Python server's
`large_insert` CI wedge exposed: one wire message's inserts ran as one
WiredTiger statement transaction, whose unevictable dirty content could in
principle cross the cache's dirty-stall threshold and livelock the engine.
Its 4G embedded default cache kept the worst 48MB-message case comfortably
inside the budget — but a daemon configured with a smaller `--cache-size`
had no such protection.

Batch inserts now commit in the same bounded chunks as the Python server
(at most 1,000 documents or 4MB per statement transaction), keeping the
dirty footprint independent of the client's batch size on any cache
configuration. As on the Python side, MongoDB batch inserts are
per-document atomic only, so the commit points are invisible to clients.

#### Fixed

- `secantus-storage`: `Storage::insert` chunks one wire batch into bounded
  statement transactions (write-conflict retry per chunk; capped-FIFO
  fresh-key protection spans the whole client batch). Pinned by
  `batch_insert.rs::large_batch_insert_survives_a_small_cache` (35k × 1.1KB
  documents against a deliberate 128M cache) plus ordered/unordered
  cross-chunk semantics tests.
