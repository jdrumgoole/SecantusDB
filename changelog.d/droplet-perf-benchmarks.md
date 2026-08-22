### Published benchmarks now measure on dedicated hardware

The per-operation latency and concurrent-writer scaling figures on
`docs/benchmark.md`, `docs/concurrency.md` and the website's performance page
were measured on a developer laptop. That is not a trustworthy place to measure
one, and the failure mode is silent: a background build or an OS indexer moves
every column at once and nothing in the output says so. One run taken straight
after a parallel compile recorded *mongod itself* at 2.5x its own baseline, and
because the workloads run sequentially while load decays, the ratios were
skewed too — it reported a fabricated 0.3x where the honest figure was 0.8x.
The table looked entirely normal.

#### Added

- `do-cluster perf` / `invoke do-perf` — runs both Python benchmark harnesses
  on a DigitalOcean droplet and pulls `bench/results/latency.json` and
  `bench/results/concurrency.json` back for the chart generators. Uses only the
  server droplet: both harnesses spawn all three engines and drive them over
  loopback, which is what makes them per-operation *engine* measurements rather
  than network measurements, so a client droplet would add nothing but a NIC.
- `bench.compare_servers --json PATH` — writes results directly in the
  `latency.json` schema. Publishing these numbers previously required
  hand-transcribing 27 figures into that file.
- `bench/DO_CLUSTER.md` documents the command, and records why `mongod` is the
  control: it is measured in the same run and does not change between releases,
  so if its numbers drift from the previous results file, the machine moved and
  not the engine.
