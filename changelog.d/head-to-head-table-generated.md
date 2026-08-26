### The published benchmark table is generated, not pasted

`docs/benchmark.md`'s "Over a real network" head-to-head table was the last
benchmark surface a human had to update by hand.

#### Added

- `bench.head_to_head_chart` rewrites it from
  `bench/results/do/<run>/comparison.md`, the artifact `release-benchmark`
  already writes, and `--check` exits non-zero when the page is stale.
- `tests/test_benchmark_table_fresh.py` runs that check, so a forgotten refresh
  fails the suite instead of silently publishing old numbers. It went stale
  twice: once leaving a post-lz4 droplet section above a pre-lz4 latency table,
  and once running two releases behind while the header directly above it named
  a different mongod version.
