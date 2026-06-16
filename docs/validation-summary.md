# Cross-Driver Conformance Summary

Generated 2026-06-15 — SecantusDB 0.5.3b7. Each per-driver gauge runs the driver vendor's own integration test suite (unmodified) against a SecantusDB daemon and emits its raw output to `.validation/`. This summary normalises on **test count** so the 8 gauges compare like for like — every row counts one assertion outcome, whether it landed as a JUnit `<testcase>`, a Mocha test, an RSpec example, a `go test` event, or a pytest collected item.

**Failures split into two columns**: *Failed* counts tests that actually need a fix on SecantusDB; *Expected* counts tests with a documented reason for failing (driver-side cascade, out-of-scope feature, single-node-topology assumption, known intermittent flake). The expected list lives in `validation_summary/expected_failures.py` and each entry carries a rationale. Adjusted pass rate = passes ÷ (passes + actual failures).

## Summary by driver

| Driver | Language | Driver version | Tests run | Passed | Failed | Expected | Skipped | Pass rate | Adjusted |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| `pymongo` | Python | `f2103a95870a` | 1504 | 1021 | 8 | 0 | 475 | 99.2% | 99.2% |
| `mongo-java-driver` | Java | `cb45be6bb147` | 900 | 447 | 0 | 0 | 453 | 100.0% | 100.0% |
| `mongo-go-driver` | Go | `fd85a834c40e` | 453 | 401 | 0 | 0 | 52 | 100.0% | 100.0% |
| `mongo-node-driver` | Node.js | `7e53685952f2` | 364 | 358 | 0 | 1 | 5 | 99.7% | 100.0% |
| `mongo-ruby-driver` | Ruby | `f68d676643c1` | 283 | 258 | 0 | 1 | 24 | 99.6% | 100.0% |
| `mongo-rust-driver` | Rust | `12dd49bf18bb` | 101 | 101 | 0 | 0 | 0 | 100.0% | 100.0% |
| `mongo-php-library` | PHP | `12e56461166d` | 2221 | 2126 | 58 | 0 | 37 | 97.3% | 97.3% |
| `mongo-php-driver` | PHP | `e81b318a33dc` | 270 | 240 | 7 | 0 | 23 | 97.2% | 97.2% |
| **All drivers** | — | — | **6096** | **4952** | **73** | **2** | **1069** | **98.5%** | **98.5%** |

## Per-driver scope

- **`pymongo`** — curated server-touching pytest paths under vendor/pymongo-tests/test/.
- **`mongo-java-driver`** — 21 of 112 driver-sync functional classes (bson codec unit tests excluded — they don't touch the server).
- **`mongo-go-driver`** — vendor/mongo-go-driver/internal/integration/....
- **`mongo-node-driver`** — curated test/integration/ spec set.
- **`mongo-ruby-driver`** — curated spec/mongo/*.rb spec files.
- **`mongo-rust-driver`** — curated driver/src/test/ in-tree tests.
- **`mongo-php-library`** — curated functional tests (Operation / Collection / Database / Command).
- **`mongo-php-driver`** — curated .phpt wire-protocol tests (bson serialization units excluded).

## Expected failures

These tests fail for documented reasons that have no SecantusDB-side fix (driver-internal behaviour we can't influence, features intentionally out of scope, single-node topology assumptions in tests that assume a 3-node replica set, etc.). Each entry has a rationale in `validation_summary/expected_failures.py`. If you fix one of these gaps, delete its entry there.

### `mongo-node-driver` (1)

- **Find should correctly sort using text search in find** — Text indexes (`$text`, `$meta: textScore`, text-index creation) are intentionally out of scope per CLAUDE.md — would require a full-text index implementation. Documented in tasks/backlog.md §4.

### `mongo-ruby-driver` (1)

- **Mongo::Collection#create when the collection has options when the collection has a write concern when write concern passed in as an option applies the write concern passed in as an option** — The test passes `w: 2` and expects success — it assumes the canonical multi-node replica-set test cluster the Ruby driver's own CI runs against. SecantusDB advertises as a single-node replica set, so `w: 2` returns `CannotSatisfyWriteConcern` (the correct mongod emulation). Documented in tasks/backlog.md §5.

## Per-driver reports

Each gauge ships its own detailed report — per-category breakdown, named failures for triage, and the gauge's own setup notes. Open the one whose pass / fail counts you want to dig into:

- [pymongo](./validation-report.md)
- [mongo-java-driver](./validation-report-java.md)
- [mongo-go-driver](./validation-report-go.md)
- [mongo-node-driver](./validation-report-node.md)
- [mongo-ruby-driver](./validation-report-ruby.md)
- [mongo-rust-driver](./validation-report-rust.md)
- [mongo-php-library](./validation-report-php-lib.md)
- [mongo-php-driver](./validation-report-php-ext.md)

## Refreshing

Run all 8 gauges plus this summary:

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
