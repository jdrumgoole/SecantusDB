# mongo-rust-driver Validation Report

Generated 2026-08-31 — SecantusDB 0.6.0b16 vs mongo-rust-driver 12dd49b (`vendor/mongo-rust-driver/`).

Run `uv run python -m invoke validate-rust` to refresh. The Rust-driver analogue of the pymongo / mongo-go-driver / mongo-node-driver / mongo-java-driver / mongo-ruby-driver gauges — the language MongoDB consumers reach for when they want native performance + async.

## Summary

| Module | Passed | Failed | Ignored | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `change_stream` | 12 | 0 | 0 | 12 | 100.0% |
| `client` | 6 | 0 | 0 | 6 | 100.0% |
| `coll` | 35 | 1 | 0 | 36 | 97.2% |
| `cursor` | 6 | 0 | 0 | 6 | 100.0% |
| `db` | 12 | 0 | 0 | 12 | 100.0% |
| `error` | 5 | 0 | 0 | 5 | 100.0% |
| `index_management` | 7 | 0 | 0 | 7 | 100.0% |
| `spec` | 21 | 1 | 0 | 22 | 95.5% |
| **Overall** | **104** | **2** | **0** | **106** | **98.1%** |

## Failures (2)

First 30 failed tests for triage:

```
test::coll::find_one_and_delete_hint_server_version
    thread 'test::coll::find_one_and_delete_hint_server_version' (14795) panicked at driver/src/test/coll.rs:663:9:
test::spec::crud::run_unified
    thread 'test::spec::crud::run_unified' (15440) panicked at driver/src/test/spec/unified_runner/test_file.rs:679:80:
```

## How this is generated

``invoke validate-rust`` spawns a SecantusDB daemon on a fresh ephemeral port, runs ``cargo test --lib -p mongodb`` against the curated include set with ``MONGODB_URI`` explicitly overridden in the subprocess env (so the user's ambient env can't leak through to a real mongod), parses cargo's per-test output, and writes this report. The list of in-scope tests lives in ``rust_validation/include_paths.py``.
