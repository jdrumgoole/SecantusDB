### The oplog prune moves off the write path entirely

Under sustained write load the oplog reaches its entry cap within seconds,
and from then on the opportunistic prune has to delete rows as fast as
they arrive. That sweep — a key merge across the shard tables, PITR
archiving, per-row deletes — ran inline on whichever thread crossed the
cadence: the writer itself in the default mode (measured at roughly a
third of the whole insert path under cap pressure), or a drainer in
async mode. A dedicated background pruner now owns the sweep in both
modes; write paths just set a flag. mongod does the same job on its
OplogCapMaintainerThread, for the same reason.

Oplog reads got cheaper alongside: shard tables are created lazily and
most never exist, but every oplog merge probed all sixteen plus the
legacy table, paying a failed cursor-open per absent table per read. A
shard-existence mask seeded at open skips them outright. The embedded
Rust `Storage` library also now defaults to the same 4G WiredTiger
cache *cap* as the daemon and the Python handle (the cache fills
lazily, so small test instances stay small) — closing the gap where a
library user hit eviction pressure at 256M that the daemon never would.

Measured on the standard concurrency methodology (8 KiB docs, batch
100, sync oplog, interleaved A/B): **+7.7% single-writer and +2.9% at
eight writers**, with every single-writer rep separating cleanly.

#### Changed

- Rust storage: the opportunistic oplog prune runs on a dedicated
  background pruner thread (signalled by the write-path cadence, with a
  10s retention backstop) instead of inline on writer / drainer
  threads. Explicit `prune_oplog` calls are unchanged (synchronous).
- Rust storage: oplog merges (reads, floor, prune scans, archiving)
  skip shard tables known absent via an existence mask seeded at open.
- Rust storage: `Storage::open`'s default WiredTiger cache is a 4G cap
  (was 256M), matching the daemon and the embedded Python handle.
