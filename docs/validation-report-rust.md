# mongo-rust-driver Validation Report

Generated 2026-07-06 — SecantusDB 0.5.4b161 vs mongo-rust-driver 12dd49b (`vendor/mongo-rust-driver/`).

Run `uv run python -m invoke validate-rust` to refresh. The Rust-driver analogue of the pymongo / mongo-go-driver / mongo-node-driver / mongo-java-driver / mongo-ruby-driver gauges — the language MongoDB consumers reach for when they want native performance + async.

## Summary

| Module | Passed | Failed | Ignored | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `bulk_write` | 1 | 0 | 0 | 1 | 100.0% |
| `change_stream` | 12 | 0 | 0 | 12 | 100.0% |
| `client` | 6 | 0 | 0 | 6 | 100.0% |
| `coll` | 36 | 0 | 0 | 36 | 100.0% |
| `cursor` | 6 | 0 | 0 | 6 | 100.0% |
| `db` | 12 | 0 | 0 | 12 | 100.0% |
| `error` | 5 | 0 | 0 | 5 | 100.0% |
| `index_management` | 7 | 0 | 0 | 7 | 100.0% |
| `spec` | 20 | 0 | 0 | 20 | 100.0% |
| **Overall** | **105** | **0** | **0** | **105** | **100.0%** |

## How this is generated

``invoke validate-rust`` spawns a SecantusDB daemon on a fresh ephemeral port, runs ``cargo test --lib -p mongodb`` against the curated include set with ``MONGODB_URI`` explicitly overridden in the subprocess env (so the user's ambient env can't leak through to a real mongod), parses cargo's per-test output, and writes this report. The list of in-scope tests lives in ``rust_validation/include_paths.py``.
