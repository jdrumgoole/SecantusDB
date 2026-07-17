### The Rust server's oplog bookkeeping gets off the write path

The Rust server's oplog housekeeping no longer taxes the hot paths. The
opportunistic oplog prune that runs on the write path (every 1000 emits,
under the writer's lock) used to decode every oplog row in full just to read
its timestamp — an O(entire-oplog) stall for every concurrent writer each
time it fired. The retention scan now peeks the timestamp out of the raw
BSON bytes without materialising the document, stops dating rows at the
first in-window entry (timestamps are monotone in seq, so the expired rows
form a prefix), and walks the rest keys-only. `hello` replies stopped
writing to storage entirely: the per-call oplog-meta persist that every
driver heartbeat used to pay — a single-row WiredTiger hotspot every
concurrent writer contended on — is gone, matching the cure the Python
server shipped in 0.5.4b236.

Crash recovery got structurally safer at the same time. The oplog meta row
is now written once at close, and recovery treats it as a hint, not the
truth: the recovered counters are clamped up past what the oplog and
natural-index tables actually contain, so a crash can never lead to a
re-minted (duplicate) oplog seq — previously a stale meta row could
overwrite live oplog rows after an unclean shutdown. Restart monotonicity of
the cluster clock is guaranteed the same way the Python server does it:
recovery bumps the clock one full second past everything it can see, which
covers any `hello`-minted timestamp that was never persisted.

#### Changed

- Rust server: `current_cluster_time` (every `hello` reply under the
  replica-set persona) no longer persists the oplog meta row, and no longer
  takes the global storage lock — it is a pure in-memory mint under the
  dedicated oplog mutex.
- Rust server: the write-path opportunistic oplog prune peeks timestamps
  from raw BSON (no full-document decode), early-stops at the first
  in-retention row, and collects the remainder of the walk keys-only;
  `startAtOperationTime` seq resolution uses the same raw peek.
- Rust server: oplog-meta recovery reconstructs from the newest oplog row
  with a single reverse cursor step instead of a full-table decode walk.

#### Fixed

- Rust server: a stale oplog-meta row (the on-disk state a crash leaves
  behind, since the meta snapshot is written at close, not per emit) can no
  longer rewind `next_seq` / `next_nat_seq` — recovery clamps both counters
  up past the table maxima, so a reopen can never re-mint an already-used
  oplog seq (which would silently overwrite a live oplog row) or collide a
  natural-order entry.
- Rust server: the cluster clock can no longer step backwards across an
  unclean restart — recovery bumps it one second past the recovered
  timestamp, the wall clock, and the oplog tail, covering mints that were
  never persisted.
