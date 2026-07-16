# Cross-Driver Conformance Summary

Generated 2026-07-16 — SecantusDB 0.5.4b234. Each per-driver gauge runs the driver vendor's own integration test suite (unmodified) against a SecantusDB daemon and emits its raw output to `.validation/`. This summary normalises on **test count** so the 0 gauges compare like for like — every row counts one assertion outcome, whether it landed as a JUnit `<testcase>`, a Mocha test, an RSpec example, a `go test` event, or a pytest collected item.

**Failures split into two columns**: *Failed* counts tests that actually need a fix on SecantusDB; *Expected* counts tests with a documented reason for failing (driver-side cascade, out-of-scope feature, single-node-topology assumption, known intermittent flake). The expected list lives in `validation_summary/expected_failures.py` and each entry carries a rationale. Adjusted pass rate = passes ÷ (passes + actual failures).

## Summary by driver

| Driver | Language | Driver version | Tests run | Passed | Failed | Expected | Skipped | Pass rate | Adjusted |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| **All drivers** | — | — | **0** | **0** | **0** | **0** | **0** | **—** | **—** |

## Per-driver scope


## Per-driver reports

Each gauge ships its own detailed report — per-category breakdown, named failures for triage, and the gauge's own setup notes. Open the one whose pass / fail counts you want to dig into:


## Refreshing

Run all 0 gauges plus this summary:

```
uv run python -m invoke validate-all
uv run python -m invoke validate-summary
```

Run a single gauge (still updates that one report) plus the summary:

```
uv run python -m invoke validate-java       # or validate / validate-go / etc.
uv run python -m invoke validate-summary
```

The summary reads whatever is currently in `.validation/`; a gauge that's never been run is silently omitted from the table.
