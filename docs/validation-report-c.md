# mongo-c-driver Validation Report

Generated 2026-06-19 — SecantusDB 0.5.4b7 vs mongo-c-driver 57dba9c049 (`vendor/mongo-c-driver/`).

Run `uv run python -m invoke validate-c` to refresh. The official MongoDB **C** driver (`libmongoc`) is the lowest-level official client — and (with the Go and PHP-extension gauges) one of the strictest wire-protocol checks.

## Summary

| Suite | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `/BulkOperation` | 92 | 1 | 10 | 103 | 98.9% |
| `/Client` | 96 | 9 | 14 | 119 | 91.4% |
| `/Collection` | 139 | 6 | 11 | 156 | 95.9% |
| `/Cursor` | 68 | 2 | 0 | 70 | 97.1% |
| `/Database` | 17 | 2 | 0 | 19 | 89.5% |
| `/ReadConcern` | 6 | 0 | 0 | 6 | 100.0% |
| `/ReadPrefs` | 16 | 0 | 0 | 16 | 100.0% |
| `/WriteCommand` | 6 | 0 | 0 | 6 | 100.0% |
| `/WriteConcern` | 11 | 0 | 2 | 13 | 100.0% |
| `/bulkwrite` | 0 | 0 | 13 | 13 | 100.0% |
| `/collection-management` | 4 | 1 | 0 | 5 | 80.0% |
| `/command_monitoring` | 33 | 1 | 1 | 35 | 97.1% |
| `/crud` | 160 | 0 | 12 | 172 | 100.0% |
| `/find_and_modify` | 8 | 0 | 1 | 9 | 100.0% |
| `/gridfs` | 10 | 0 | 1 | 11 | 100.0% |
| `/gridfs_old` | 32 | 0 | 2 | 34 | 100.0% |
| `/index-management` | 3 | 3 | 0 | 6 | 50.0% |
| `/long_namespace` | 6 | 1 | 2 | 9 | 85.7% |
| **Overall** | **707** | **26** | **69** | **802** | **96.5%** |

## Failures (26)

First 30 failed tests for triage:

```
/Client/ipv6/single
/Client/ipv6/single
/Client/command_w_write_concern
/Client/select_server/single
/Client/select_server/pooled
/Client/select_server/err/single
/Client/select_server/err/pooled
/Client/last_write_date_absent
/Client/last_write_date_absent/pooled
/BulkOperation/OP_MSG/max_msg_size
/Collection/index
/Collection/index_w_write_concern
/Collection/drop
/Collection/aggregate/bypass_document_validation
/Collection/rename
/Collection/tailable/timeout/single
/command_monitoring/unified/writeConcernError
/Cursor/error_document/getmore
/Cursor/batchsize_override_decimal128
/Database/create_with_write_concern
/Database/drop
/long_namespace/unsupported_long_db
/collection-management/modifyCollection-errorResponse
/index-management/updateSearchIndex
/index-management/listSearchIndexes
/index-management/dropSearchIndex
```

## How this is generated

`invoke validate-c` builds the vendored driver's `test-libmongoc` binary once (CMake, `ENABLE_TESTS=ON`), spawns a SecantusDB daemon on a fresh ephemeral port, and runs the curated `-l` prefixes with `MONGOC_TEST_URI` pointed at the daemon, writing JSON results via `-F`. The list of in-scope test prefixes (and the skip-list of out-of-scope tests) lives in `c_validation/include_paths.py`.
