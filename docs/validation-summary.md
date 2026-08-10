# Cross-Driver Conformance Summary

Generated 2026-08-10 — SecantusDB 0.6.0b9. Each per-driver gauge runs the driver vendor's own integration test suite (unmodified) against a SecantusDB daemon and emits its raw output to `.validation/`. This summary normalises on **test count** so the 13 gauges compare like for like — every row counts one assertion outcome, whether it landed as a JUnit `<testcase>`, a Mocha test, an RSpec example, a `go test` event, or a pytest collected item.

**Failures split into two columns**: *Failed* counts tests that actually need a fix on SecantusDB; *Expected* counts tests with a documented reason for failing (driver-side cascade, out-of-scope feature, single-node-topology assumption, known intermittent flake). The expected list lives in `validation_summary/expected_failures.py` and each entry carries a rationale. Adjusted pass rate = passes ÷ (passes + actual failures).

## Summary by driver

| Driver | Language | Driver version | Tests run | Passed | Failed | Expected | Skipped | Pass rate | Adjusted |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| `pymongo` | Python | `f2103a95870a` | 1501 | 1018 | 8 | 0 | 475 | 99.2% | 99.2% |
| `pymongo (async)` | Python | `f2103a95870a` | 1423 | 923 | 9 | 0 | 491 | 99.0% | 99.0% |
| `mongo-java-driver` | Java | `cb45be6bb147` | 900 | 445 | 1 | 1 | 453 | 99.6% | 99.8% |
| `mongo-kotlin-driver` | Kotlin | `cb45be6bb147` | 538 | 294 | 0 | 0 | 244 | 100.0% | 100.0% |
| `mongo-go-driver` | Go | `fd85a834c40e` | 453 | 401 | 0 | 0 | 52 | 100.0% | 100.0% |
| `mongo-node-driver` | Node.js | `7e53685952f2` | 364 | 358 | 0 | 1 | 5 | 99.7% | 100.0% |
| `mongo-ruby-driver` | Ruby | `f68d676643c1` | 283 | 258 | 0 | 1 | 24 | 99.6% | 100.0% |
| `mongo-rust-driver` | Rust | `12dd49bf18bb` | 105 | 105 | 0 | 0 | 0 | 100.0% | 100.0% |
| `mongo-php-library` | PHP | `12e56461166d` | 2221 | 2145 | 39 | 0 | 37 | 98.2% | 98.2% |
| `mongo-php-driver` | PHP | `e81b318a33dc` | 270 | 247 | 0 | 0 | 23 | 100.0% | 100.0% |
| `mongo-c-driver` | C | `57dba9c04991` | 841 | 739 | 3 | 8 | 91 | 98.5% | 99.6% |
| `mongo-cxx-driver` | C++ | `24852b68a3d1` | 899 | 890 | 0 | 0 | 9 | 100.0% | 100.0% |
| `mongo-csharp-driver` | C# | `8297e62d7f2b` | 228 | 202 | 0 | 0 | 26 | 100.0% | 100.0% |
| **All drivers** | — | — | **10026** | **8025** | **60** | **11** | **1930** | **99.1%** | **99.3%** |

## Per-driver scope

- **`pymongo`** — curated server-touching pytest paths under vendor/pymongo-tests/test/.
- **`pymongo (async)`** — AsyncMongoClient suite under vendor/pymongo-tests/test/asynchronous/.
- **`mongo-java-driver`** — driver-sync functional integration tests.
- **`mongo-kotlin-driver`** — driver-kotlin-sync integrationTest (ships in the mongo-java-driver monorepo).
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

### `mongo-java-driver` (1)

- **client metadata is not propagated to the server: metadata append does not create new connections or close existing ones and no hello command is sent** — ClientMetadataTest: asserts the driver does NOT open a new connection or re-send `hello` after a client-side `appendMetadata` call. `appendMetadata` crosses no wire — purely Java-driver connection/handshake logic. Not server-fixable.

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

Run all 13 gauges plus this summary:

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
