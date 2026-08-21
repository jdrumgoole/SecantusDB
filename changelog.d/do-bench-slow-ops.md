### Record slow operations with timestamps, and what that found

`do-client --slow-ms MS` records every operation at or above a threshold with
its completion timestamp, worker id and operation type. A latency histogram
deliberately throws time away, which is precisely what a tail investigation
needs back: whether slow operations arrive periodically (a checkpoint, a prune,
a flush) or at random (lock contention, eviction) is the first fork in the
diagnosis, and only timestamps answer it. It costs nothing when off, which is
the default.

It was added to chase the p99.9 gap the MongoDB comparison exposed — SecantusDB
runs at roughly half MongoDB's throughput but with **9.8-11.3x its p99.9
latency**, a far worse ratio than at the median. The traces localised it
quickly, and the full finding with its evidence is recorded in
`tasks/backlog.md`:

- The **read path is clean** — 8 concurrent readers see a 2x spread from p50 to
  p99.9. There is no read tail.
- The **write path owns it**, and it is a concurrency effect rather than a
  per-write cost: the tail jumps 7x between 2 and 4 concurrent writers
  (0.68ms → 4.83ms) while the median barely moves.
- Every large stall hits **all** workers within the same few milliseconds, at
  irregular intervals — a convoy behind a shared resource, not a periodic
  background task.
- Checkpoints, WiredTiger log preallocation, and log file size were each ruled
  out by experiment; oplog pruning turned out to drive the worst single outlier
  (126ms → 26ms when disabled) but not p99.9.
- The root cause is **WiredTiger cache pressure**: the tail scales inversely
  with cache headroom (52x spread at a 512M cache, 7x at 8G) while throughput
  and median latency stay flat. Shrinking the documents instead of growing the
  cache does the same thing, so it is the rate dirty data fills the cache that
  governs the tail.
- There is **no WiredTiger config-only fix**. Every eviction knob tried either
  did nothing (thread count) or made the tail worse while costing throughput
  (dirty and updates thresholds).
- On the droplets both engines ran the **same 4G cache**, yet mongod held p99.9
  at 10.75ms where SecantusDB reached 121ms. The structural difference is
  admission control: MongoDB bounds concurrent storage-engine write
  transactions so excess writers queue outside the engine, while SecantusDB
  lets every connection thread dive straight into WiredTiger. The harness data
  prices the trade — capping writers at 4 rather than 16 costs 23% of
  throughput and halves the tail.
- `--oplog-async` recovers a further 24% of the tail and removes 42% of the
  stalls, independently of the cache story.

#### Added

- `--slow-ms` on `do-client run`, surfaced as `slow_ops` in the client report.
