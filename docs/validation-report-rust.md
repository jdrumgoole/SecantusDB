# mongo-rust-driver Validation Report

Measured 2026-10-09 (raw artifact), generated 2026-10-09 — SecantusDB 0.7.0b2 vs mongo-rust-driver 12dd49bf (`vendor/mongo-rust-driver/`).

Run `uv run python -m invoke validate-rust` to refresh. The Rust-driver analogue of the pymongo / mongo-go-driver / mongo-node-driver / mongo-java-driver / mongo-ruby-driver gauges — the language MongoDB consumers reach for when they want native performance + async.

## Summary

| Module | Passed | Failed | Ignored | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `change_stream` | 11 | 0 | 0 | 11 | 100.0% |
| `client` | 6 | 0 | 0 | 6 | 100.0% |
| `coll` | 32 | 1 | 0 | 33 | 96.9% |
| `cursor` | 6 | 0 | 0 | 6 | 100.0% |
| `db` | 12 | 0 | 0 | 12 | 100.0% |
| `error` | 5 | 0 | 0 | 5 | 100.0% |
| `index_management` | 7 | 0 | 0 | 7 | 100.0% |
| `spec` | 21 | 0 | 0 | 21 | 100.0% |
| **Overall** | **100** | **1** | **0** | **101** | **99.0%** |

## Failures (1)

First 30 failed tests for triage:

```
test::coll::find_one_and_delete_hint_server_version
    thread 'test::coll::find_one_and_delete_hint_server_version' (3443559) panicked at driver/src/test/coll.rs:663:9:
```

## How this is generated

``invoke validate-rust`` spawns a SecantusDB daemon on a fresh ephemeral port, runs ``cargo test --lib -p mongodb`` against the curated include set with ``MONGODB_URI`` explicitly overridden in the subprocess env (so the user's ambient env can't leak through to a real mongod), parses cargo's per-test output, and writes this report. The list of in-scope tests lives in ``rust_validation/include_paths.py``.
