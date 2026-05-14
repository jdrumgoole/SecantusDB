# Cross-Driver Conformance Summary

Generated 2026-05-15 — SecantusDB 0.5.1b8. Each per-driver gauge runs the driver vendor's own integration test suite (unmodified) against a SecantusDB daemon and emits its raw output to `.validation/`. This summary normalises on **test count** so the five gauges compare like for like — every row counts one assertion outcome, whether it landed as a JUnit `<testcase>`, a Mocha test, an RSpec example, a `go test` event, or a pytest collected item.

## Summary by driver

| Driver | Language | Driver version | Tests run | Passed | Failed | Skipped | Pass rate |
|---|---|---|---:|---:|---:|---:|---:|
| `pymongo` | Python | `f2103a95870a` | 1341 | 959 | 0 | 382 | 100.0% |
| `mongo-java-driver` | Java | `cb45be6bb147` | 4710 | 4244 | 2 | 464 | 100.0% |
| `mongo-go-driver` | Go | `fd85a834c40e` | 453 | 398 | 3 | 52 | 99.3% |
| `mongo-node-driver` | Node.js | `7e53685952f2` | 364 | 358 | 1 | 5 | 99.7% |
| `mongo-ruby-driver` | Ruby | `f68d676643c1` | 318 | 291 | 3 | 24 | 99.0% |
| **All drivers** | — | — | **7186** | **6250** | **9** | **927** | **99.9%** |

## Per-driver scope

- **`pymongo`** — curated pytest paths under vendor/pymongo-tests/test/.
- **`mongo-java-driver`** — 21 of 112 driver-sync functional classes + bson unit tests.
- **`mongo-go-driver`** — vendor/mongo-go-driver/internal/integration/....
- **`mongo-node-driver`** — curated test/integration/ spec set.
- **`mongo-ruby-driver`** — curated spec/mongo/*.rb spec files.

## Per-driver reports

Each gauge ships its own detailed report — per-category breakdown, named failures for triage, and the gauge's own setup notes. Open the one whose pass / fail counts you want to dig into:

- [pymongo](./validation-report.md)
- [mongo-java-driver](./validation-report-java.md)
- [mongo-go-driver](./validation-report-go.md)
- [mongo-node-driver](./validation-report-node.md)
- [mongo-ruby-driver](./validation-report-ruby.md)

## Refreshing

Run all five gauges plus this summary:

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
