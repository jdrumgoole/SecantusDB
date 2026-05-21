# mongo-rust-driver Validation Report

Generated 2026-05-21 — SecantusDB 0.5.2b5 vs mongo-rust-driver 12dd49bf (`vendor/mongo-rust-driver/`).

Run `uv run python -m invoke validate-rust` to refresh. The Rust-driver analogue of the pymongo / mongo-go-driver / mongo-node-driver / mongo-java-driver / mongo-ruby-driver gauges — the language MongoDB consumers reach for when they want native performance + async.

## Summary

| Module | Passed | Failed | Ignored | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `client` | 1 | 2 | 0 | 3 | 33.3% |
| `coll` | 17 | 0 | 0 | 17 | 100.0% |
| `db` | 4 | 0 | 0 | 4 | 100.0% |
| **Overall** | **22** | **2** | **0** | **24** | **91.7%** |

## Failures (2)

First 30 failed tests for triage:

```
test::client::list_databases
    thread 'test::client::list_databases' (607641827) panicked at driver/src/test/client.rs:215:9:
test::client::metadata_sent_in_handshake
    thread 'test::client::metadata_sent_in_handshake' (607641929) panicked at driver/src/test/client.rs:80:10:
```

## How this is generated

``invoke validate-rust`` spawns a SecantusDB daemon on a fresh ephemeral port, runs ``cargo test --lib -p mongodb`` against the curated include set with ``MONGODB_URI`` explicitly overridden in the subprocess env (so the user's ambient env can't leak through to a real mongod), parses cargo's per-test output, and writes this report. The list of in-scope tests lives in ``rust_validation/include_paths.py``.
