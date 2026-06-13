# pymongo Validation Report (Rust server)

Generated 2026-06-14 — SecantusDB 0.5.2b33 vs pymongo f2103a95870a (`vendor/pymongo-tests/`).

Run `uv run python -m invoke validate --server rust` to refresh. This is the R8 conformance gate from `tasks/rust-server-plan.md`: the same unmodified pymongo suite the headline gauge runs, pointed at the **Rust server** instead of the pure-Python one. The gap between this pass rate and `docs/validation-report.md` is the Rust server's remaining to-do list.

## Summary by category

| Category | Passed | Failed | Errored | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|---:|
| `test_binary.py` | 29 | 0 | 0 | 0 | 29 | 100.0% |
| `test_bulk.py` | 29 | 5 | 0 | 4 | 38 | 85.3% |
| `test_change_stream.py` | 70 | 36 | 0 | 49 | 155 | 66.0% |
| `test_collation.py` | 15 | 1 | 0 | 0 | 16 | 93.8% |
| `test_collection.py` | 65 | 22 | 0 | 4 | 91 | 74.7% |
| `test_collection_management.py` | 4 | 3 | 0 | 0 | 7 | 57.1% |
| `test_command_logging.py` | 22 | 0 | 0 | 14 | 36 | 100.0% |
| `test_command_monitoring.py` | 30 | 1 | 0 | 7 | 38 | 96.8% |
| `test_comment.py` | 3 | 0 | 0 | 0 | 3 | 100.0% |
| `test_common.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_crud_unified.py` | 229 | 72 | 0 | 185 | 486 | 76.1% |
| `test_crud_v1.py` | 14 | 0 | 0 | 0 | 14 | 100.0% |
| `test_cursor.py` | 51 | 16 | 0 | 5 | 72 | 76.1% |
| `test_custom_types.py` | 46 | 5 | 0 | 0 | 51 | 90.2% |
| `test_database.py` | 28 | 7 | 0 | 1 | 36 | 80.0% |
| `test_decimal128.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_examples.py` | 11 | 7 | 0 | 2 | 20 | 61.1% |
| `test_logger.py` | 4 | 0 | 0 | 2 | 6 | 100.0% |
| `test_operations.py` | 2 | 0 | 0 | 0 | 2 | 100.0% |
| `test_raw_bson.py` | 13 | 1 | 0 | 0 | 14 | 92.9% |
| `test_read_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| `test_read_preferences.py` | 9 | 0 | 0 | 20 | 29 | 100.0% |
| `test_results.py` | 5 | 0 | 0 | 0 | 5 | 100.0% |
| `test_run_command.py` | 15 | 1 | 0 | 5 | 21 | 93.8% |
| `test_transactions_unified.py` | 69 | 26 | 0 | 172 | 267 | 72.6% |
| `test_versioned_api.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_versioned_api_integration.py` | 36 | 2 | 0 | 5 | 43 | 94.7% |
| `test_write_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| **Overall** | **823** | **205** | **0** | **475** | **1503** | **80.1%** |

## Failures (205)

First 30 failure node-ids for manual triage:

```
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_large_inserts_ordered
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_large_inserts_unordered
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_update_many_pipeline
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_update_one_pipeline
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_upsert_uuid_standard_subdocuments
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_raises_error_on_missing_id_418plus
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_read_concern
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_resumetoken_uniterated_nonempty_batch_resumeafter
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_resumetoken_uniterated_nonempty_batch_startafter
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_split_large_change
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_start_after
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_update_resume_token
vendor/pymongo-tests/test/test_change_stream.py::TestDatabaseChangeStream::test_start_after
vendor/pymongo-tests/test/test_change_stream.py::TestDatabaseChangeStream::test_start_after_resume_process_with_changes
vendor/pymongo-tests/test/test_change_stream.py::TestDatabaseChangeStream::test_start_after_resume_process_without_changes
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreams::test_Change_Stream_should_allow_valid_aggregate_pipeline_stages
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreams::test_Test_array_truncation
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreams::test_Test_modified_structure_in_ns_document_MUST_NOT_err
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreams::test_Test_newField_added_in_response_MUST_NOT_err
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreams::test_Test_new_structure_in_ns_document_MUST_NOT_err
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreams::test_Test_projection_in_change_stream_returns_expected_fields
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreams::test_Test_server_error_on_projecting_out__id
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreams::test_Test_unknown_operationType_MUST_NOT_err
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsErrors::test_Change_Stream_should_error_when__id_is_projected_out
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsErrors::test_Change_Stream_should_error_when_an_invalid_aggregation_stage_is_passed_in
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsPreAndPostImages::test_fullDocument:required_with_changeStreamPreAndPostImages_disabled
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsPreAndPostImages::test_fullDocument:required_with_changeStreamPreAndPostImages_enabled
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsPreAndPostImages::test_fullDocument:whenAvailable_with_changeStreamPreAndPostImages_disabled
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsPreAndPostImages::test_fullDocument:whenAvailable_with_changeStreamPreAndPostImages_enabled
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsPreAndPostImages::test_fullDocumentBeforeChange:off_with_changeStreamPreAndPostImages_disabled
```
... and 175 more (see raw JSON).

## How this is generated

**pymongo's tests are run unmodified.** The submodule at `vendor/pymongo-tests/` is checked out at the pinned upstream tag with zero local edits — `git diff HEAD` inside the submodule is empty. The integration is entirely external: `pymongo_validation/plugin.py` starts an embedded Rust server (`_secantus_server.RustServer(storage_path=<fresh tempdir>, port=0)` — the in-process Rust accept loop over the pure-Rust engines and WiredTiger-backed storage; Python is only the launcher) (real on-disk WiredTiger via `tempfile.mkdtemp(prefix='secantus-pymongo-gauge-')`, not `:memory:`) in `pytest_configure` and writes the bound host/port into `DB_IP` + `DB_PORT` — the env vars pymongo's own `helpers_shared.py` reads at import time. Pytest then collects and runs the in-scope test paths defined in `pymongo_validation/include_paths.py`.

Tests gated on replica-set / sharding / auth / TLS / encryption topology self-skip — those skips are honest gaps, not failures. The pass rate above is therefore a meaningful conformance number: those are pymongo's actual tests, exercising SecantusDB the same way they exercise a real `mongod` in pymongo's CI.
