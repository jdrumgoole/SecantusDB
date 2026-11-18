### The website no longer presents the Python servers

The deprecated Python servers are gone from secantusdb.com. `/python-db.html`
now redirects to the Rust MongoDB page; the home page drops its Python card
and "Where the Python servers fit" section; the performance page and the
driver-conformance grid show the Rust server against `mongod` only.

#### Changed

- `bench/latency_chart.py` and `bench/concurrency_chart.py` leave the Python
  series out of the site charts (the latency chart is rescaled to fit); the
  docs charts in `docs/benchmark.md` and `docs/concurrency.md` keep all three.
- `validation_summary.driver_panels` renders only the Rust grid, and
  `--server` now defaults to `rust`.
