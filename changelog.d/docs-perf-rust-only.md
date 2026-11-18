### The performance docs compare the Rust server with mongod only

The benchmark and concurrency pages no longer chart, tabulate or discuss
the deprecated Python server. Only the Rust server is compared with
`mongod`, as on the website's performance page.

#### Changed

- `bench/latency_chart.py` and `bench/concurrency_chart.py` no longer
  render the Python series on either surface. A `python` entry in a
  results file is ignored.
- `docs/benchmark.md` and `docs/concurrency.md`: the Python columns,
  chart lines and prose are gone.
