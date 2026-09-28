### The Go gauge reported 100.0% over a run that stopped three-quarters of the way through

`go test` killing itself on `-timeout` panics the binary **without** emitting a
terminal event for the tests still in flight. The summariser therefore counted
only the tests that finished, found no failures among them, and printed 100.0%
— while the package-level event in the same file said `fail`. The two
disagreed, and nothing compared them.

Every recorded run of this gauge had been truncated at the same point: 476 of
481 tests, on **both** servers, including the committed 2026-09-21 report. The
five that never complete are `TestInitialDNSSeedlistDiscoverySpec` and two
others, which resolve SRV/TXT records against `mongodb.test.build.10gen.cc` —
DNS tests that never open a connection to SecantusDB, and which cost 30 minutes
of wall clock per run before the binary gives up.

So the published "Go: 100%" has never described the whole include set. What it
did describe is real: 439 of 439 tests that ran passed.

#### Fixed

- The report generator now detects a truncated run — a test that emitted `run`
  with no terminal event, or a package reporting `fail` with no failing test
  beneath it — and refuses to present the rate as a result: the report opens
  with a banner naming the tests that never finished, and the process exits
  non-zero.

  The banner goes **above** the table on purpose. A reader who meets the numbers
  first has already formed a view of the pass rate by the time a footnote
  reaches them.

- An ordinary red run (a package `fail` **with** a failing test under it) is
  deliberately not flagged. A guard that cried truncation at every failing gauge
  would be ignored exactly when it mattered.

#### Added

- `tests/test_go_gauge_truncation.py`, the Go counterpart of the pgjdbc guard in
  `tests/test_pgjdbc_gauge_truncation.py` — which already existed, and which the
  Go gauge simply never had.

#### How it was found

A fresh Rust-server run and a stale report from a different server, a different
version and a week earlier carried **identical** numbers. Agreement between a
fresh artifact and a stale one reads as confirmation; here it meant both were
cut off at the same DNS hang. The honest signal was the wall clock: 30m 09s for
a gauge whose own timeout is 30m is a truncation, not a slow run.

---

### A crashed pymongo gauge published a fabricated report

Separate bug, same class, found in the same run. The two pymongo tasks run
pytest with `warn=True` — correctly, since a partially-red suite still owes us a
report — and then generated the report **unconditionally**. Unlike `_run_gauge`,
they never cleared the raw artifact first.

So `invoke validate --server rust` died in **one second** on a missing
`_secantus_server`, collected zero tests, and rewrote
`docs/validation-report-rust-server.md` as *"Generated 2026-09-28 — SecantusDB
0.6.0b17"*, **99.4%** — over a raw artifact from **30 August**.

The figures had even drifted against the previous report (99.5% → 99.4%, with a
new failing test) because the generator changed under the same data. A reader
diffing the file would have concluded a fresh run had caught a regression.

#### Fixed

- Both pymongo tasks now clear the raw before the run and refuse to generate a
  report when the run produced none, leaving the previous report untouched and
  naming the usual cause (`invoke rust-server-build`).

#### Also found, not fixed here

Seven `-rust-server` reports claim *"Generated 2026-09-21 — 0.6.0b16"* over raw
artifacts dated 10–30 August: pymongo, pymongo-async, c, cxx, dotnet, php-ext
and php-lib. The date records when the generator ran, not when the tests did.
Filed in `tasks/backlog.md`.
