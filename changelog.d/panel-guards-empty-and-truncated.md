### Two more ways the driver panels could publish a number they had not measured

Both found while rendering the first Rust-server grid, and both the same shape as
the Go and pymongo bugs fixed earlier the same day: an artifact that looks
current and isn't.

#### Fixed

- **An EMPTY artifact passed the freshness check.** It tests mtime, and an empty
  directory keeps a fresh one — so `java-results-rust-server/`, cleared by a
  re-run and never refilled, had the *newest* timestamp of the whole set and
  rendered a blank panel beside twelve real ones. The report on disk still
  described the data that had gone. The guard now refuses an artifact that
  exists but holds no results, naming it.

- **The panel showed a rate for a truncated run.** #1613 stopped the Go *report*
  claiming one; the panel had no such check and published **100.0%** — over a run
  cut short at 476 of 481 tests by a 30-minute DNS hang. `passed / ran` over the
  part that finished looks *better* the more tests went missing, which is what
  made the number so convincing. `GaugeStats.truncated` now carries the signal
  and the panel renders an em-dash instead.

The Go collector detects truncation by the same two signatures the report
generator uses: a test that emitted `run` with no terminal event, or a package
reporting `fail` with no failing test beneath it. Verified against both real Go
artifacts — python and rust — which are both flagged.
