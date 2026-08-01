### Concurrency graphs are now generated, refreshed per release

The N-writer scaling charts on secantusdb.com/performance and in the
docs' concurrency deep-dive are no longer hand-authored SVG. A new
`invoke concurrency-refresh` task re-measures all four series (Python
server, Rust server, Rust async stack, mongod) with `bench.concurrency`
— now able to drive the async-oplog stack directly (`--server
rust-async`), take medians over interleaved runs (`--runs`), and write
machine-readable results (`--json`) — and `bench.concurrency_chart`
regenerates the chart and data-table blocks in both surfaces from those
results. The committed results live at `bench/results/concurrency.json`,
and a test pins the committed charts to exactly what that file renders
to, so the graphs can no longer silently drift from the measurements.
The refresh is part of the per-release website update.

#### Added
- `bench.concurrency`: `--server rust-async` (async + non-logged oplog
  stack), `--runs N` interleaved-median sweeps, and `--json PATH`
  structured output; `--server all` now sweeps four servers.
- `bench.concurrency_chart`: renders the website and docs concurrency
  chart + table blocks from the results JSON into marker-delimited
  regions.
- `invoke concurrency-refresh`: benchmark + regenerate in one step
  (`--skip-bench` re-renders from the committed results).
- `tests/test_concurrency_chart.py`: pins the render/replace logic and
  fails if the committed charts are stale relative to the committed
  results JSON.
