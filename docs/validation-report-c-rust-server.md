# mongo-c-driver Validation Report

Generated 2026-08-10 — SecantusDB 0.6.0b9 vs mongo-c-driver 57dba9c (`vendor/mongo-c-driver/`).

Run `uv run python -m invoke validate-c` to refresh. The official MongoDB **C** driver (`libmongoc`) is the lowest-level official client — and (with the Go and PHP-extension gauges) one of the strictest wire-protocol checks.

## Summary

| Suite | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `/BulkOperation` | 93 | 0 | 10 | 103 | 100.0% |
| `/Client` | 96 | 6 | 19 | 121 | 94.1% |
| `/Collection` | 146 | 0 | 11 | 157 | 100.0% |
| `/Cursor` | 70 | 0 | 0 | 70 | 100.0% |
| `/Database` | 19 | 0 | 0 | 19 | 100.0% |
| `/ReadConcern` | 6 | 0 | 0 | 6 | 100.0% |
| `/ReadPrefs` | 16 | 0 | 0 | 16 | 100.0% |
| `/WriteCommand` | 6 | 0 | 0 | 6 | 100.0% |
| `/WriteConcern` | 13 | 0 | 0 | 13 | 100.0% |
| `/change_stream` | 21 | 2 | 2 | 25 | 91.3% |
| `/change_streams` | 3 | 1 | 0 | 4 | 75.0% |
| `/collection-management` | 5 | 0 | 0 | 5 | 100.0% |
| `/command_monitoring` | 33 | 1 | 1 | 35 | 97.1% |
| `/crud` | 160 | 0 | 12 | 172 | 100.0% |
| `/find_and_modify` | 9 | 0 | 0 | 9 | 100.0% |
| `/gridfs` | 10 | 0 | 1 | 11 | 100.0% |
| `/gridfs_old` | 32 | 0 | 2 | 34 | 100.0% |
| `/long_namespace` | 8 | 0 | 1 | 9 | 100.0% |
| **Overall** | **746** | **10** | **59** | **815** | **98.7%** |

## Failures (10)

First 30 failed tests for triage:

```
/change_stream/resume_at_optime
/change_stream/resume_with_post_batch_resume_token
/Client/ipv6/single
/Client/ipv6/single
/Client/select_server/single
/Client/select_server/pooled
/Client/select_server/err/single
/Client/select_server/err/pooled
/command_monitoring/unified/writeConcernError
/change_streams/unified/change-streams-resume-errorLabels
```

## How this is generated

`invoke validate-c` builds the vendored driver's `test-libmongoc` binary once (CMake, `ENABLE_TESTS=ON`), spawns a SecantusDB daemon on a fresh ephemeral port, and runs the curated `-l` prefixes with `MONGOC_TEST_URI` pointed at the daemon, writing JSON results via `-F`. The list of in-scope test prefixes (and the skip-list of out-of-scope tests) lives in `c_validation/include_paths.py`.
