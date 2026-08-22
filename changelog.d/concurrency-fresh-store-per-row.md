### Concurrency sweep gives each row its own store

`bench.concurrency` shared one store across the whole writer sweep, so every
row measured a different database. With 8,192-byte documents the 1-, 2- and
4-writer rows leave tens of gigabytes behind, and the 8-writer row wrote into a
tree several times the size the 1-writer row saw. In a measurement whose sole
purpose is to isolate writer count, that is a confound — and it biases scaling
*downwards*, because later rows look worse partly for having a bigger tree.

It was also a hard failure. On a 48 GB droplet the accumulated store exhausted
the disk mid-sweep and WiredTiger took its documented ENOSPC panic — "the
process must exit and restart: WT_PANIC" — killing the writers. The harness
correctly refused to report a row measured with a missing writer rather than
publishing a silently-low number.

Each row now provisions a fresh store and server, bounding peak disk to a
single row.

#### Changed

- Published concurrency figures are **not comparable across this change**: rows
  after the first previously carried the accumulated weight of every row before
  them, so scaling was understated. The next published sweep re-baselines.
