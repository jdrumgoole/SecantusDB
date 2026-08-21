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
- `--oplog-async` recovers 24% of the tail and removes 42% of the stalls,
  implicating the process-wide oplog mutex as one strand of the convoy.

#### Added

- `--slow-ms` on `do-client run`, surfaced as `slow_ops` in the client report.
