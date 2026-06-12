# pymongo Validation Report (Rust server)

Generated 2026-06-12 — SecantusDB 0.5.2b16 vs pymongo f2103a95870a (`vendor/pymongo-tests/`).

Run `uv run python -m invoke validate --server rust` to refresh. This is the R8 conformance gate from `tasks/rust-server-plan.md`: the same unmodified pymongo suite the headline gauge runs, pointed at the **Rust server** instead of the pure-Python one. The gap between this pass rate and `docs/validation-report.md` is the Rust server's remaining to-do list.

## Summary by category

| Category | Passed | Failed | Errored | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|---:|
| `test_binary.py` | 29 | 0 | 0 | 0 | 29 | 100.0% |
| `test_bson.py` | 87 | 0 | 0 | 1 | 88 | 100.0% |
| `test_bson_corpus.py` | 31 | 0 | 0 | 0 | 31 | 100.0% |
| `test_bulk.py` | 29 | 5 | 0 | 4 | 38 | 85.3% |
| `test_change_stream.py` | 3 | 106 | 0 | 46 | 155 | 2.8% |
| `test_code.py` | 8 | 0 | 0 | 0 | 8 | 100.0% |
| `test_collation.py` | 15 | 1 | 0 | 0 | 16 | 93.8% |
| `test_collection.py` | 63 | 24 | 0 | 4 | 91 | 72.4% |
| `test_collection_management.py` | 4 | 3 | 0 | 0 | 7 | 57.1% |
| `test_command_logging.py` | 22 | 0 | 0 | 14 | 36 | 100.0% |
| `test_command_monitoring.py` | 30 | 1 | 0 | 7 | 38 | 96.8% |
| `test_comment.py` | 3 | 0 | 0 | 0 | 3 | 100.0% |
| `test_common.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_crud_unified.py` | 217 | 84 | 0 | 185 | 486 | 72.1% |
| `test_crud_v1.py` | 14 | 0 | 0 | 0 | 14 | 100.0% |
| `test_cursor.py` | 47 | 20 | 0 | 5 | 72 | 70.1% |
| `test_custom_types.py` | 37 | 14 | 0 | 0 | 51 | 72.5% |
| `test_database.py` | 28 | 7 | 0 | 1 | 36 | 80.0% |
| `test_dbref.py` | 9 | 3 | 0 | 0 | 12 | 75.0% |
| `test_decimal128.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_default_exports.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| `test_errors.py` | 8 | 0 | 0 | 0 | 8 | 100.0% |
| `test_examples.py` | 8 | 10 | 0 | 2 | 20 | 44.4% |
| `test_json_util.py` | 24 | 0 | 0 | 0 | 24 | 100.0% |
| `test_logger.py` | 4 | 0 | 0 | 2 | 6 | 100.0% |
| `test_objectid.py` | 15 | 0 | 0 | 0 | 15 | 100.0% |
| `test_operations.py` | 2 | 0 | 0 | 0 | 2 | 100.0% |
| `test_raw_bson.py` | 13 | 1 | 0 | 0 | 14 | 92.9% |
| `test_read_concern.py` | 5 | 1 | 0 | 0 | 6 | 83.3% |
| `test_read_preferences.py` | 9 | 0 | 0 | 20 | 29 | 100.0% |
| `test_results.py` | 5 | 0 | 0 | 0 | 5 | 100.0% |
| `test_run_command.py` | 14 | 2 | 0 | 5 | 21 | 87.5% |
| `test_son.py` | 11 | 0 | 0 | 0 | 11 | 100.0% |
| `test_timestamp.py` | 7 | 0 | 0 | 0 | 7 | 100.0% |
| `test_versioned_api.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_versioned_api_integration.py` | 36 | 2 | 0 | 5 | 43 | 94.7% |
| `test_write_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| **Overall** | **861** | **284** | **0** | **301** | **1446** | **75.2%** |

## Failures (284)

First 30 failure node-ids for manual triage:

```
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_large_inserts_ordered
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_large_inserts_unordered
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_update_many_pipeline
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_update_one_pipeline
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_upsert_uuid_standard_subdocuments
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_aggregate_cursor_blocks
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_batch_size_is_honored
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_change_operations
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_concurrent_close
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_full_pipeline
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_iteration
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_next_blocks
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_simple
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_start_after
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_start_after_resume_process_with_changes
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_start_after_resume_process_without_changes
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_start_at_operation_time
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_try_next
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_try_next_runs_one_getmore
vendor/pymongo-tests/test/test_change_stream.py::TestClusterChangeStream::test_watch
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_aggregate_cursor_blocks
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_batch_size_is_honored
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_change_operations
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_concurrent_close
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_document_id_order
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_full_pipeline
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_initial_empty_batch
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_iteration
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_kill_cursors
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_next_blocks
```
... and 254 more (see raw JSON).

## How this is generated

**pymongo's tests are run unmodified.** The submodule at `vendor/pymongo-tests/` is checked out at the pinned upstream tag with zero local edits — `git diff HEAD` inside the submodule is empty. The integration is entirely external: `pymongo_validation/plugin.py` starts an embedded Rust server (`_secantus_server.RustServer(storage_path=<fresh tempdir>, port=0)` — the in-process Rust accept loop over the pure-Rust engines and WiredTiger-backed storage; Python is only the launcher) (real on-disk WiredTiger via `tempfile.mkdtemp(prefix='secantus-pymongo-gauge-')`, not `:memory:`) in `pytest_configure` and writes the bound host/port into `DB_IP` + `DB_PORT` — the env vars pymongo's own `helpers_shared.py` reads at import time. Pytest then collects and runs the in-scope test paths defined in `pymongo_validation/include_paths.py`.

Tests gated on replica-set / sharding / auth / TLS / encryption topology self-skip — those skips are honest gaps, not failures. The pass rate above is therefore a meaningful conformance number: those are pymongo's actual tests, exercising SecantusDB the same way they exercise a real `mongod` in pymongo's CI.
