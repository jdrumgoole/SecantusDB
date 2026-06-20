# Cross-Driver Conformance Summary

Generated 2026-06-20 — SecantusDB 0.5.4b9. Each per-driver gauge runs the driver vendor's own integration test suite (unmodified) against a SecantusDB daemon and emits its raw output to `.validation/`. This summary normalises on **test count** so the 11 gauges compare like for like — every row counts one assertion outcome, whether it landed as a JUnit `<testcase>`, a Mocha test, an RSpec example, a `go test` event, or a pytest collected item.

**Failures split into two columns**: *Failed* counts tests that actually need a fix on SecantusDB; *Expected* counts tests with a documented reason for failing (driver-side cascade, out-of-scope feature, single-node-topology assumption, known intermittent flake). The expected list lives in `validation_summary/expected_failures.py` and each entry carries a rationale. Adjusted pass rate = passes ÷ (passes + actual failures).

## Summary by driver

| Driver | Language | Driver version | Tests run | Passed | Failed | Expected | Skipped | Pass rate | Adjusted |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| `pymongo` | Python | `f2103a95870a` | 1504 | 1021 | 8 | 0 | 475 | 99.2% | 99.2% |
| `mongo-java-driver` | Java | `cb45be6bb147` | 900 | 447 | 0 | 0 | 453 | 100.0% | 100.0% |
| `mongo-go-driver` | Go | `fd85a834c40e` | 453 | 398 | 0 | 3 | 52 | 99.3% | 100.0% |
| `mongo-node-driver` | Node.js | `7e53685952f2` | 364 | 358 | 0 | 1 | 5 | 99.7% | 100.0% |
| `mongo-ruby-driver` | Ruby | `f68d676643c1` | 283 | 258 | 0 | 1 | 24 | 99.6% | 100.0% |
| `mongo-rust-driver` | Rust | `12dd49bf18bb` | 101 | 101 | 0 | 0 | 0 | 100.0% | 100.0% |
| `mongo-php-library` | PHP | `12e56461166d` | 2221 | 2147 | 37 | 0 | 37 | 98.3% | 98.3% |
| `mongo-php-driver` | PHP | `e81b318a33dc` | 270 | 246 | 1 | 0 | 23 | 99.6% | 99.6% |
| `mongo-c-driver` | C | `57dba9c04991` | 802 | 712 | 13 | 8 | 69 | 97.1% | 98.2% |
| `mongo-cxx-driver` | C++ | `24852b68a3d1` | 897 | 885 | 3 | 0 | 9 | 99.7% | 99.7% |
| `mongo-csharp-driver` | C# | `8297e62d7f2b` | 228 | 202 | 0 | 0 | 26 | 100.0% | 100.0% |
| **All drivers** | — | — | **8023** | **6775** | **62** | **13** | **1173** | **98.9%** | **99.1%** |

## Per-driver scope

- **`pymongo`** — curated server-touching pytest paths under vendor/pymongo-tests/test/.
- **`mongo-java-driver`** — 21 of 112 driver-sync functional classes (bson codec unit tests excluded — they don't touch the server).
- **`mongo-go-driver`** — vendor/mongo-go-driver/internal/integration/....
- **`mongo-node-driver`** — curated test/integration/ spec set.
- **`mongo-ruby-driver`** — curated spec/mongo/*.rb spec files.
- **`mongo-rust-driver`** — curated driver/src/test/ in-tree tests.
- **`mongo-php-library`** — curated functional tests (Operation / Collection / Database / Command).
- **`mongo-php-driver`** — curated .phpt wire-protocol tests (bson serialization units excluded).
- **`mongo-c-driver`** — curated test-libmongoc wire-protocol suites (CRUD / cursor / aggregate / command).
- **`mongo-cxx-driver`** — curated mongocxx test_driver Catch2 suite (CRUD / cursor / aggregate / gridfs).
- **`mongo-csharp-driver`** — curated MongoDB.Driver.Tests CRUD specification suite (xUnit).

## Expected failures

These tests fail for documented reasons that have no SecantusDB-side fix (driver-internal behaviour we can't influence, features intentionally out of scope, single-node topology assumptions in tests that assume a 3-node replica set, etc.). Each entry has a rationale in `validation_summary/expected_failures.py`. If you fix one of these gaps, delete its entry there.

### `mongo-go-driver` (3)

- **TestChangeStream_ReplicaSet/try_next/one_getMore_sent** — Long-standing intermittent flake — fails ~1/3 runs with `TryNext returned true on iteration 1`. Repros only under full-gauge load. Cause unknown after focused investigation. Documented in tasks/backlog.md §5.
- **TestChangeStream_ReplicaSet/try_next** — Rollup of the `one_getMore_sent` subtest above.
- **TestChangeStream_ReplicaSet** — Rollup of the `try_next/one_getMore_sent` subtest above.

### `mongo-node-driver` (1)

- **Find should correctly sort using text search in find** — Text indexes (`$text`, `$meta: textScore`, text-index creation) are intentionally out of scope per CLAUDE.md — would require a full-text index implementation. Documented in tasks/backlog.md §4.

### `mongo-ruby-driver` (1)

- **Mongo::Collection#create when the collection has options when the collection has a write concern when write concern passed in as an option applies the write concern passed in as an option** — The test passes `w: 2` and expects success — it assumes the canonical multi-node replica-set test cluster the Ruby driver's own CI runs against. SecantusDB advertises as a single-node replica set, so `w: 2` returns `CannotSatisfyWriteConcern` (the correct mongod emulation). Documented in tasks/backlog.md §5.

### `mongo-c-driver` (8)

- **/Client/ipv6/single** — Requires an IPv6 listener (`MONGOC_TEST_IPV6`); the gauge daemon binds IPv4 `127.0.0.1` only. Environment-specific, not a protocol gap.
- **/Client/ipv6/single** — Requires an IPv6 listener (`MONGOC_TEST_IPV6`); the gauge daemon binds IPv4 `127.0.0.1` only. Environment-specific, not a protocol gap.
- **/Client/select_server/single** — libmongoc asserts the selected server is standalone / mongos / RS-secondary, but SecantusDB advertises itself as an RS *primary* in `hello` (deliberate — pymongo's change-stream topology machinery needs a replica-set primary). RSPrimary fails the test's `is_standalone_or_(rs_secondary_or_)mongos` check. A consequence of the single-node-replica-set advertisement, not a CRUD/wire gap.
- **/Client/select_server/pooled** — libmongoc asserts the selected server is standalone / mongos / RS-secondary, but SecantusDB advertises itself as an RS *primary* in `hello` (deliberate — pymongo's change-stream topology machinery needs a replica-set primary). RSPrimary fails the test's `is_standalone_or_(rs_secondary_or_)mongos` check. A consequence of the single-node-replica-set advertisement, not a CRUD/wire gap.
- **/Client/select_server/err/single** — libmongoc asserts the selected server is standalone / mongos / RS-secondary, but SecantusDB advertises itself as an RS *primary* in `hello` (deliberate — pymongo's change-stream topology machinery needs a replica-set primary). RSPrimary fails the test's `is_standalone_or_(rs_secondary_or_)mongos` check. A consequence of the single-node-replica-set advertisement, not a CRUD/wire gap.
- **/Client/select_server/err/pooled** — libmongoc asserts the selected server is standalone / mongos / RS-secondary, but SecantusDB advertises itself as an RS *primary* in `hello` (deliberate — pymongo's change-stream topology machinery needs a replica-set primary). RSPrimary fails the test's `is_standalone_or_(rs_secondary_or_)mongos` check. A consequence of the single-node-replica-set advertisement, not a CRUD/wire gap.
- **/Client/last_write_date_absent** — Asserts `lastWriteDate` is absent (a standalone trait); SecantusDB advertises as an RS primary and so returns `lastWrite.lastWriteDate` in `hello`. Same root cause as the `/Client/select_server` entries — the single-node-replica-set advertisement.
- **/Client/last_write_date_absent/pooled** — Asserts `lastWriteDate` is absent (a standalone trait); SecantusDB advertises as an RS primary and so returns `lastWrite.lastWriteDate` in `hello`. Same root cause as the `/Client/select_server` entries — the single-node-replica-set advertisement.

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
- [mongo-c-driver](./validation-report-c.md)
- [mongo-cxx-driver](./validation-report-cxx.md)
- [mongo-csharp-driver](./validation-report-dotnet.md)

## Refreshing

Run all 11 gauges plus this summary:

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
