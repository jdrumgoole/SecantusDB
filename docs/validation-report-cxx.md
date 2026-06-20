# mongo-cxx-driver Validation Report

Generated 2026-06-20 — SecantusDB 0.5.4b9 vs mongo-cxx-driver 24852b6 (`vendor/mongo-cxx-driver/`).

Run `uv run python -m invoke validate-cxx` to refresh. The official MongoDB **C++** driver (`mongocxx`), built on libmongoc — its Catch2 `test_driver` suite (CRUD / cursor / aggregate / GridFS / commands) run unmodified against an embedded SecantusDB daemon.

## Summary

| Passed | Failed | Skipped | Total | Pass rate |
|---:|---:|---:|---:|---:|
| 885 | 3 | 9 | 897 | 99.7% |

(Catch2 expands each `SECTION` into its own JUnit `<testcase>`, so the total exceeds the number of `TEST_CASE`s.)

## Failures (3)

First 30 failed tests for triage:

```
Spec Prose Tests/1. ChangeStream must continuously track the last seen resumeToken
integration tests for client metadata handshake feature/with client
integration tests for client metadata handshake feature/with pool
```

## How this is generated

`invoke validate-cxx` builds the vendored libmongoc (installed to a prefix) and the mongocxx `test_driver` Catch2 binary against it, then binds a SecantusDB daemon on `127.0.0.1:27017` (mongocxx's core tests hard-wire the driver default port — there's no `MONGOC_TEST_URI`-style override) and runs the suite with Catch2's JUnit reporter. Out-of-scope tags (CSFLE, Atlas, transactions, sessions, SDAM monitoring) are excluded in `cxx_validation/include_paths.py`.
