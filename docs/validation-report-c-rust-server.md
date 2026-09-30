# mongo-c-driver Validation Report

Generated 2026-09-30 — SecantusDB 0.6.0b17 vs mongo-c-driver 57dba9c049 (`vendor/mongo-c-driver/`).

Run `uv run python -m invoke validate-c` to refresh. The official MongoDB **C** driver (`libmongoc`) is the lowest-level official client — and (with the Go and PHP-extension gauges) one of the strictest wire-protocol checks.

## Summary

| Suite | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `/BulkOperation` | 93 | 0 | 10 | 103 | 100.0% |
| `/Client` | 102 | 2 | 15 | 119 | 98.0% |
| `/Collection` | 144 | 0 | 12 | 156 | 100.0% |
| `/Cursor` | 70 | 0 | 0 | 70 | 100.0% |
| `/Database` | 19 | 0 | 0 | 19 | 100.0% |
| `/ReadConcern` | 6 | 0 | 0 | 6 | 100.0% |
| `/ReadPrefs` | 16 | 0 | 0 | 16 | 100.0% |
| `/WriteCommand` | 6 | 0 | 0 | 6 | 100.0% |
| `/WriteConcern` | 13 | 0 | 0 | 13 | 100.0% |
| `/bulkwrite` | 12 | 0 | 1 | 13 | 100.0% |
| `/change_stream` | 23 | 0 | 2 | 25 | 100.0% |
| `/change_streams` | 11 | 0 | 0 | 11 | 100.0% |
| `/collection-management` | 5 | 0 | 0 | 5 | 100.0% |
| `/command_monitoring` | 34 | 0 | 1 | 35 | 100.0% |
| `/crud` | 171 | 0 | 1 | 172 | 100.0% |
| `/find_and_modify` | 9 | 0 | 0 | 9 | 100.0% |
| `/gridfs` | 10 | 0 | 1 | 11 | 100.0% |
| `/gridfs_old` | 32 | 0 | 2 | 34 | 100.0% |
| `/index-management` | 6 | 0 | 0 | 6 | 100.0% |
| `/long_namespace` | 8 | 0 | 1 | 9 | 100.0% |
| **Overall** | **790** | **2** | **46** | **838** | **99.7%** |

## Failures (2)

First 30 failed tests for triage:

```
/Client/ipv6/single
/Client/ipv6/single
```

## How this is generated

`invoke validate-c` builds the vendored driver's `test-libmongoc` binary once (CMake, `ENABLE_TESTS=ON`) and runs it TWICE, each against a fresh SecantusDB daemon on an ephemeral port with `MONGOC_TEST_URI` pointed at it, writing JSON results via `-F`: the curated `-l` prefixes against the default single-node replica set, then the few tests that assert standalone semantics (`STANDALONE_ONLY`) against a `--standalone` daemon. The two are merged into one result set. The in-scope prefixes, the skip-list of out-of-scope tests and the standalone list live in `c_validation/include_paths.py`.
