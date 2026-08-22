### Re-measure the published benchmark at release time

`docs/benchmark.md` publishes a head-to-head comparison against a real
`mongod`. It is prose with numbers in it, so it goes stale silently: nothing in
the test suite fails when the engine gets faster, and a release that improves
performance ships a page that understates it.

That is not hypothetical. The figures published there were measured the day
before lz4 replaced zlib as the block compressor — so without this step, the
release that nearly doubled write throughput would have shipped the old
numbers.

`invoke release-benchmark` provisions the three droplets, deploys both engines,
runs three interleaved passes on incompressible payloads, prints the comparison
table, and destroys the cluster. Two settings are deliberate: `--payload random`
(both engines compress, so the default payload measures the compressor and
flatters whichever side compresses harder — this alone moved the published
ratio from 0.46x to 0.27x) and `--repeat 3` (a single pass carries no spread,
so nothing tells you whether the median is worth quoting).

#### Added

- `invoke release-benchmark`, and an "At release time" section in
  `bench/DO_CLUSTER.md` describing when to run it and why those two settings
  are not negotiable.
