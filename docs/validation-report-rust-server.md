# pymongo Validation Report (Rust server)

Generated 2026-08-30 — SecantusDB 0.6.0b16 vs pymongo f2103a95870a (`vendor/pymongo-tests/`).

Run `uv run python -m invoke validate --server rust` to refresh. This is the R8 conformance gate from `tasks/rust-server-plan.md`: the same unmodified pymongo suite the headline gauge runs, pointed at the **Rust server** instead of the pure-Python one. The gap between this pass rate and `docs/validation-report.md` is the Rust server's remaining to-do list.

## Summary by category

| Category | Passed | Failed | Errored | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|---:|
| `test_binary.py` | 29 | 0 | 0 | 0 | 29 | 100.0% |
| `test_bulk.py` | 34 | 0 | 0 | 4 | 38 | 100.0% |
| `test_change_stream.py` | 109 | 0 | 0 | 46 | 155 | 100.0% |
| `test_collation.py` | 16 | 0 | 0 | 0 | 16 | 100.0% |
| `test_collection.py` | 85 | 2 | 0 | 4 | 91 | 97.7% |
| `test_collection_management.py` | 7 | 0 | 0 | 0 | 7 | 100.0% |
| `test_command_logging.py` | 22 | 0 | 0 | 14 | 36 | 100.0% |
| `test_command_monitoring.py` | 32 | 0 | 0 | 6 | 38 | 100.0% |
| `test_comment.py` | 3 | 0 | 0 | 0 | 3 | 100.0% |
| `test_common.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_crud_unified.py` | 344 | 0 | 0 | 142 | 486 | 100.0% |
| `test_crud_v1.py` | 14 | 0 | 0 | 0 | 14 | 100.0% |
| `test_cursor.py` | 64 | 3 | 0 | 5 | 72 | 95.5% |
| `test_custom_types.py` | 51 | 0 | 0 | 0 | 51 | 100.0% |
| `test_database.py` | 35 | 0 | 0 | 1 | 36 | 100.0% |
| `test_decimal128.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_examples.py` | 18 | 0 | 0 | 2 | 20 | 100.0% |
| `test_logger.py` | 4 | 0 | 0 | 2 | 6 | 100.0% |
| `test_operations.py` | 2 | 0 | 0 | 0 | 2 | 100.0% |
| `test_raw_bson.py` | 14 | 0 | 0 | 0 | 14 | 100.0% |
| `test_read_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| `test_read_preferences.py` | 9 | 0 | 0 | 20 | 29 | 100.0% |
| `test_results.py` | 5 | 0 | 0 | 0 | 5 | 100.0% |
| `test_run_command.py` | 16 | 0 | 0 | 5 | 21 | 100.0% |
| `test_transactions_unified.py` | 95 | 0 | 0 | 169 | 264 | 100.0% |
| `test_versioned_api.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_versioned_api_integration.py` | 38 | 1 | 0 | 4 | 43 | 97.4% |
| `test_write_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| **Overall** | **1070** | **6** | **0** | **424** | **1500** | **99.4%** |

## Failures (6)

First 30 failure node-ids for manual triage:

```
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_index_hashed
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_index_text
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_maxtime_ms_message
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_to_list_csot_applied
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_where
vendor/pymongo-tests/test/test_versioned_api_integration.py::TestVersionedApiCrudApiVersion_1::test_client_bulkWrite_appends_declared_API_version
```

## How this is generated

**pymongo's tests are run unmodified.** The submodule at `vendor/pymongo-tests/` is checked out at the pinned upstream tag with zero local edits — `git diff HEAD` inside the submodule is empty. The integration is entirely external: `pymongo_validation/plugin.py` starts an embedded Rust server (`_secantus_server.RustServer(storage_path=<fresh tempdir>, port=0)` — the in-process Rust accept loop over the pure-Rust engines and WiredTiger-backed storage; Python is only the launcher) (real on-disk WiredTiger via `tempfile.mkdtemp(prefix='secantus-pymongo-gauge-')`, not `:memory:`) in `pytest_configure` and writes the bound host/port into `DB_IP` + `DB_PORT` — the env vars pymongo's own `helpers_shared.py` reads at import time. Pytest then collects and runs the in-scope test paths defined in `pymongo_validation/include_paths.py`.

Tests gated on replica-set / sharding / auth / TLS / encryption topology self-skip — those skips are honest gaps, not failures. The pass rate above is therefore a meaningful conformance number: those are pymongo's actual tests, exercising SecantusDB the same way they exercise a real `mongod` in pymongo's CI.
