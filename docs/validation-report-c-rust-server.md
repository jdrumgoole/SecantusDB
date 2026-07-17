# mongo-c-driver Validation Report

Generated 2026-07-17 — SecantusDB 0.5.4b237 vs mongo-c-driver 57dba9c049 (`vendor/mongo-c-driver/`).

Run `uv run python -m invoke validate-c` to refresh. The official MongoDB **C** driver (`libmongoc`) is the lowest-level official client — and (with the Go and PHP-extension gauges) one of the strictest wire-protocol checks.

## Summary

| Suite | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `/BulkOperation` | 92 | 1 | 10 | 103 | 98.9% |
| `/Client` | 95 | 10 | 14 | 119 | 90.5% |
| `/Collection` | 144 | 1 | 11 | 156 | 99.3% |
| `/Cursor` | 70 | 0 | 0 | 70 | 100.0% |
| `/Database` | 19 | 0 | 0 | 19 | 100.0% |
| `/ReadConcern` | 6 | 0 | 0 | 6 | 100.0% |
| `/ReadPrefs` | 16 | 0 | 0 | 16 | 100.0% |
| `/WriteCommand` | 6 | 0 | 0 | 6 | 100.0% |
| `/WriteConcern` | 11 | 0 | 2 | 13 | 100.0% |
| `/bulkwrite` | 0 | 0 | 13 | 13 | 100.0% |
| `/collection-management` | 5 | 0 | 0 | 5 | 100.0% |
| `/command_monitoring` | 33 | 1 | 1 | 35 | 97.1% |
| `/crud` | 160 | 0 | 12 | 172 | 100.0% |
| `/find_and_modify` | 8 | 0 | 1 | 9 | 100.0% |
| `/gridfs` | 10 | 0 | 1 | 11 | 100.0% |
| `/gridfs_old` | 32 | 0 | 2 | 34 | 100.0% |
| `/index-management` | 4 | 2 | 0 | 6 | 66.7% |
| `/long_namespace` | 7 | 0 | 2 | 9 | 100.0% |
| **Overall** | **718** | **15** | **69** | **802** | **98.0%** |

## Failures (15)

First 30 failed tests for triage:

```
/Client/ipv6/single
/Client/ipv6/single
/Client/select_server/single
/Client/select_server/pooled
/Client/select_server/err/single
/Client/select_server/err/pooled
/Client/last_write_date_absent
/Client/last_write_date_absent/pooled
/BulkOperation/OP_MSG/max_msg_size
/Collection/tailable/timeout/single
/command_monitoring/unified/writeConcernError
/Client/exhaust_cursor/single
/Client/exhaust_cursor/pool
/index-management/updateSearchIndex
/index-management/dropSearchIndex
```

## How this is generated

`invoke validate-c` builds the vendored driver's `test-libmongoc` binary once (CMake, `ENABLE_TESTS=ON`), spawns a SecantusDB daemon on a fresh ephemeral port, and runs the curated `-l` prefixes with `MONGOC_TEST_URI` pointed at the daemon, writing JSON results via `-F`. The list of in-scope test prefixes (and the skip-list of out-of-scope tests) lives in `c_validation/include_paths.py`.
