### The concurrency chart shows documents per second, without the async stack

The write-concurrency chart (docs, the website's performance page, the PyCon
deck) now plots each server's throughput in documents per second against the
number of writers: at eight writers `mongod` 8.0.38 writes 19.7k docs/s and
`secantusd-mdb` 10.2k. It used to plot each server against its own
single-writer rate (3.4× and 2.6×), which read as a comparison of the two.

#### Changed

- `bench/concurrency_chart.py` draws absolute docs/s on a zero-based axis.
- The "async stack" line (`--oplog-async --oplog-nonlogged`) is withdrawn.
  The 2026-11-18 run used it on a store whose data tables were not logged,
  where the async drainer is not used and nothing reached WiredTiger's log:
  a crash lost every acknowledged write (RM-731, now refused by the server).
  `bench/concurrency.py` runs the stack with `--data-logged`, as documented,
  and `docs/concurrency.md` says the stack applies only there.
- `bench/concurrency.py` records the run's date, host and each server's
  version (`secantusd-mdb --version`, with its source tree, and
  `mongod --version`) in the results file. The 2026-11-18 results recorded
  neither version.
