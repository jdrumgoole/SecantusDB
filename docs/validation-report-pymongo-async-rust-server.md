# pymongo async Validation Report (Rust server)

Generated 2026-08-24 — SecantusDB 0.6.0b15 vs pymongo f2103a95870a (`vendor/pymongo-tests/test/asynchronous/`).

Run `uv run python -m invoke validate-pymongo-async --server rust` to refresh. This is the async-driver analogue of the R8 conformance gate: pymongo's native `AsyncMongoClient` suite pointed at the **Rust server**.

## Summary by test file

| Test file | Passed | Failed | Errored | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|---:|
| `test_bulk.py` | 34 | 0 | 0 | 4 | 38 | 100.0% |
| `test_change_stream.py` | 97 | 0 | 0 | 58 | 155 | 100.0% |
| `test_collation.py` | 16 | 0 | 0 | 0 | 16 | 100.0% |
| `test_collection.py` | 85 | 2 | 0 | 4 | 91 | 97.7% |
| `test_collection_management.py` | 7 | 0 | 0 | 0 | 7 | 100.0% |
| `test_command_logging.py` | 22 | 0 | 0 | 14 | 36 | 100.0% |
| `test_command_monitoring.py` | 31 | 0 | 0 | 7 | 38 | 100.0% |
| `test_comment.py` | 3 | 0 | 0 | 0 | 3 | 100.0% |
| `test_common.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_crud_unified.py` | 301 | 0 | 0 | 185 | 486 | 100.0% |
| `test_cursor.py` | 57 | 3 | 0 | 12 | 72 | 95.0% |
| `test_custom_types.py` | 51 | 0 | 0 | 0 | 51 | 100.0% |
| `test_database.py` | 35 | 0 | 0 | 1 | 36 | 100.0% |
| `test_examples.py` | 18 | 0 | 0 | 2 | 20 | 100.0% |
| `test_logger.py` | 4 | 0 | 0 | 2 | 6 | 100.0% |
| `test_read_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| `test_read_preferences.py` | 9 | 1 | 0 | 20 | 30 | 90.0% |
| `test_run_command.py` | 16 | 0 | 0 | 5 | 21 | 100.0% |
| `test_transactions_unified.py` | 92 | 0 | 0 | 172 | 264 | 100.0% |
| `test_versioned_api_integration.py` | 38 | 0 | 0 | 5 | 43 | 100.0% |
| **Overall** | **926** | **6** | **0** | **491** | **1423** | **99.4%** |

## Failures (6)

First 30 failure node-ids for manual triage:

```
vendor/pymongo-tests/test/asynchronous/test_collection.py::AsyncTestCollection::test_index_hashed
vendor/pymongo-tests/test/asynchronous/test_collection.py::AsyncTestCollection::test_index_text
vendor/pymongo-tests/test/asynchronous/test_cursor.py::TestCursor::test_maxtime_ms_message
vendor/pymongo-tests/test/asynchronous/test_cursor.py::TestCursor::test_to_list_csot_applied
vendor/pymongo-tests/test/asynchronous/test_cursor.py::TestCursor::test_where
vendor/pymongo-tests/test/asynchronous/test_read_preferences.py::TestMongosAndReadPreference::test_read_preference_hedge_deprecated
```

## How this is generated

**pymongo's async tests are run unmodified.** The submodule at `vendor/pymongo-tests/` is checked out at the pinned upstream tag with zero local edits. The integration is entirely external: the shared `pymongo_validation/plugin.py` starts an embedded Rust server (`_secantus_server.RustServer(storage_path=<fresh tempdir>, port=0)`) (real on-disk WiredTiger) before pymongo's conftest is imported and writes the bound host/port into `DB_IP` + `DB_PORT` — the env vars pymongo's `helpers_shared.py` reads at import time, which the async `AsyncClientContext` also resolves from. Pytest then runs the in-scope paths from `pymongo_async_validation/include_paths.py` under `pytest-asyncio` (`asyncio_mode=auto`).

Tests gated on replica-set / sharding / auth / TLS / encryption topology self-skip — those skips are honest gaps, not failures. The pass rate is a meaningful conformance number for SecantusDB's behaviour under pymongo's async driver, exercised the same way pymongo's own CI exercises a real `mongod`.
