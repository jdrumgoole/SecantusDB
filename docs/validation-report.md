# pymongo Validation Report

Generated 2026-06-13 — SecantusDB 0.5.2b26 vs pymongo f2103a95870a (`vendor/pymongo-tests/`).

Run `uv run python -m invoke validate` to refresh. The pass rate is the best honest measure of how close SecantusDB is to a complete MongoDB surrogate for the in-scope wire-protocol surface; gaps are the to-do list.

## Summary by category

| Category | Passed | Failed | Errored | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|---:|
| `test_binary.py` | 29 | 0 | 0 | 0 | 29 | 100.0% |
| `test_bson.py` | 87 | 0 | 0 | 1 | 88 | 100.0% |
| `test_bson_corpus.py` | 31 | 0 | 0 | 0 | 31 | 100.0% |
| `test_bulk.py` | 33 | 1 | 0 | 4 | 38 | 97.1% |
| `test_change_stream.py` | 100 | 6 | 0 | 49 | 155 | 94.3% |
| `test_code.py` | 8 | 0 | 0 | 0 | 8 | 100.0% |
| `test_collation.py` | 16 | 0 | 0 | 0 | 16 | 100.0% |
| `test_collection.py` | 80 | 7 | 0 | 4 | 91 | 92.0% |
| `test_collection_management.py` | 5 | 2 | 0 | 0 | 7 | 71.4% |
| `test_command_logging.py` | 22 | 0 | 0 | 14 | 36 | 100.0% |
| `test_command_monitoring.py` | 30 | 1 | 0 | 7 | 38 | 96.8% |
| `test_comment.py` | 3 | 0 | 0 | 0 | 3 | 100.0% |
| `test_common.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_crud_unified.py` | 301 | 0 | 0 | 185 | 486 | 100.0% |
| `test_crud_v1.py` | 14 | 0 | 0 | 0 | 14 | 100.0% |
| `test_cursor.py` | 58 | 9 | 0 | 5 | 72 | 86.6% |
| `test_custom_types.py` | 49 | 2 | 0 | 0 | 51 | 96.1% |
| `test_database.py` | 34 | 1 | 0 | 1 | 36 | 97.1% |
| `test_dbref.py` | 9 | 3 | 0 | 0 | 12 | 75.0% |
| `test_decimal128.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_default_exports.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| `test_errors.py` | 8 | 0 | 0 | 0 | 8 | 100.0% |
| `test_examples.py` | 18 | 0 | 0 | 2 | 20 | 100.0% |
| `test_json_util.py` | 24 | 0 | 0 | 0 | 24 | 100.0% |
| `test_logger.py` | 4 | 0 | 0 | 2 | 6 | 100.0% |
| `test_objectid.py` | 15 | 0 | 0 | 0 | 15 | 100.0% |
| `test_operations.py` | 2 | 0 | 0 | 0 | 2 | 100.0% |
| `test_raw_bson.py` | 14 | 0 | 0 | 0 | 14 | 100.0% |
| `test_read_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| `test_read_preferences.py` | 9 | 0 | 0 | 20 | 29 | 100.0% |
| `test_results.py` | 5 | 0 | 0 | 0 | 5 | 100.0% |
| `test_run_command.py` | 16 | 0 | 0 | 5 | 21 | 100.0% |
| `test_son.py` | 11 | 0 | 0 | 0 | 11 | 100.0% |
| `test_timestamp.py` | 7 | 0 | 0 | 0 | 7 | 100.0% |
| `test_transactions_unified.py` | 92 | 3 | 0 | 172 | 267 | 96.8% |
| `test_versioned_api.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_versioned_api_integration.py` | 38 | 0 | 0 | 5 | 43 | 100.0% |
| `test_write_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| **Overall** | **1202** | **35** | **0** | **476** | **1713** | **97.2%** |

## Failures (35)

First 30 failure node-ids for manual triage:

```
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_upsert_uuid_standard_subdocuments
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_resumetoken_uniterated_nonempty_batch_resumeafter
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_resumetoken_uniterated_nonempty_batch_startafter
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsShowExpandedEvents::test_when_showExpandedEvents_is_true,_create_events_are_reported
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsShowExpandedEvents::test_when_showExpandedEvents_is_true,_create_events_on_views_are_reported
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsShowExpandedEvents::test_when_showExpandedEvents_is_true,_modify_events_are_reported
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsShowExpandedEvents::test_when_showExpandedEvents_is_true,_new_fields_on_change_stream_events_are_handled_appropriately
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_error_code
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_exhaust
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_index_dont_drop_dups
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_index_filter
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_index_hashed
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_index_text
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_min_query
vendor/pymongo-tests/test/test_collection_management.py::TestCollectionManagementClusteredIndexes::test_listCollections_includes_clusteredIndex
vendor/pymongo-tests/test/test_collection_management.py::TestCollectionManagementClusteredIndexes::test_listIndexes_returns_the_index
vendor/pymongo-tests/test/test_command_monitoring.py::TestCommandMonitoringFind::test_A_successful_find_with_showRecordId_and_returnKey
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_comment
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_max
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_maxtime_ms_message
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_min
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_tailable
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_to_list_csot_applied
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_to_list_tailable
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_where
vendor/pymongo-tests/test/test_cursor.py::TestRawBatchCommandCursor::test_exhaust_cursor_db_set
vendor/pymongo-tests/test/test_custom_types.py::TestCollectionWCustomType::test_aggregate_w_custom_type_decoder
vendor/pymongo-tests/test/test_custom_types.py::TestCollectionWCustomType::test_find_one_and__w_custom_type_decoder
vendor/pymongo-tests/test/test_database.py::TestDatabase::test_drop_collection
vendor/pymongo-tests/test/test_dbref.py::TestDBRefSpec::test_decoding_1_2_3
```
... and 5 more (see raw JSON).

## How this is generated

**pymongo's tests are run unmodified.** The submodule at `vendor/pymongo-tests/` is checked out at the pinned upstream tag with zero local edits — `git diff HEAD` inside the submodule is empty. The integration is entirely external: `pymongo_validation/plugin.py` starts an embedded `SecantusDBServer(host='127.0.0.1', port=0, storage_path=<fresh tempdir>)` (real on-disk WiredTiger via `tempfile.mkdtemp(prefix='secantus-pymongo-gauge-')`, not `:memory:`) in `pytest_configure` and writes the bound host/port into `DB_IP` + `DB_PORT` — the env vars pymongo's own `helpers_shared.py` reads at import time. Pytest then collects and runs the in-scope test paths defined in `pymongo_validation/include_paths.py`.

Tests gated on replica-set / sharding / auth / TLS / encryption topology self-skip — those skips are honest gaps, not failures. The pass rate above is therefore a meaningful conformance number: those are pymongo's actual tests, exercising SecantusDB the same way they exercise a real `mongod` in pymongo's CI.
