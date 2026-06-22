# pymongo Validation Report (Rust server)

Generated 2026-06-22 — SecantusDB 0.5.4b9 vs pymongo f2103a95870a (`vendor/pymongo-tests/`).

Run `uv run python -m invoke validate --server rust` to refresh. This is the R8 conformance gate from `tasks/rust-server-plan.md`: the same unmodified pymongo suite the headline gauge runs, pointed at the **Rust server** instead of the pure-Python one. The gap between this pass rate and `docs/validation-report.md` is the Rust server's remaining to-do list.

## Summary by category

| Category | Passed | Failed | Errored | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|---:|
| `test_binary.py` | 29 | 0 | 0 | 0 | 29 | 100.0% |
| `test_bulk.py` | 33 | 1 | 0 | 4 | 38 | 97.1% |
| `test_change_stream.py` | 104 | 2 | 0 | 49 | 155 | 98.1% |
| `test_collation.py` | 16 | 0 | 0 | 0 | 16 | 100.0% |
| `test_collection.py` | 83 | 4 | 0 | 4 | 91 | 95.4% |
| `test_collection_management.py` | 7 | 0 | 0 | 0 | 7 | 100.0% |
| `test_command_logging.py` | 22 | 0 | 0 | 14 | 36 | 100.0% |
| `test_command_monitoring.py` | 31 | 0 | 0 | 7 | 38 | 100.0% |
| `test_comment.py` | 3 | 0 | 0 | 0 | 3 | 100.0% |
| `test_common.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_crud_unified.py` | 300 | 1 | 0 | 185 | 486 | 99.7% |
| `test_crud_v1.py` | 14 | 0 | 0 | 0 | 14 | 100.0% |
| `test_cursor.py` | 61 | 6 | 0 | 5 | 72 | 91.0% |
| `test_custom_types.py` | 51 | 0 | 0 | 0 | 51 | 100.0% |
| `test_database.py` | 34 | 1 | 0 | 1 | 36 | 97.1% |
| `test_decimal128.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_examples.py` | 18 | 0 | 0 | 2 | 20 | 100.0% |
| `test_logger.py` | 4 | 0 | 0 | 2 | 6 | 100.0% |
| `test_operations.py` | 2 | 0 | 0 | 0 | 2 | 100.0% |
| `test_raw_bson.py` | 14 | 0 | 0 | 0 | 14 | 100.0% |
| `test_read_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| `test_read_preferences.py` | 9 | 0 | 0 | 20 | 29 | 100.0% |
| `test_results.py` | 5 | 0 | 0 | 0 | 5 | 100.0% |
| `test_run_command.py` | 16 | 0 | 0 | 5 | 21 | 100.0% |
| `test_transactions_unified.py` | 92 | 3 | 0 | 172 | 267 | 96.8% |
| `test_versioned_api.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_versioned_api_integration.py` | 38 | 0 | 0 | 5 | 43 | 100.0% |
| `test_write_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| **Overall** | **1010** | **18** | **0** | **475** | **1503** | **98.2%** |

## Failures (18)

First 30 failure node-ids for manual triage:

```
vendor/pymongo-tests/test/test_bulk.py::TestBulk::test_upsert_uuid_standard_subdocuments
vendor/pymongo-tests/test/test_change_stream.py::TestCollectionChangeStream::test_split_large_change
vendor/pymongo-tests/test/test_change_stream.py::TestUnifiedChangeStreamsShowExpandedEvents::test_when_showExpandedEvents_is_true,_new_fields_on_change_stream_events_are_handled_appropriately
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_exhaust
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_index_filter
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_index_hashed
vendor/pymongo-tests/test/test_collection.py::TestCollection::test_index_text
vendor/pymongo-tests/test/test_crud_unified.py::TestUnifiedAggregateLet::test_Aggregate_with_let_option
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_maxtime_ms_message
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_to_list_csot_applied
vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_where
vendor/pymongo-tests/test/test_cursor.py::TestRawBatchCommandCursor::test_aggregate_raw_snapshot_reads
vendor/pymongo-tests/test/test_cursor.py::TestRawBatchCommandCursor::test_exhaust_cursor_db_set
vendor/pymongo-tests/test/test_cursor.py::TestRawBatchCursor::test_find_raw_snapshot_reads
vendor/pymongo-tests/test/test_database.py::TestDatabase::test_list_collection_names
vendor/pymongo-tests/test/test_transactions_unified.py::TestUnifiedReadPref::test_secondary_readPreference
vendor/pymongo-tests/test/test_transactions_unified.py::TestUnifiedRunCommand::test_run_command_fails_with_explicit_secondary_read_preference
vendor/pymongo-tests/test/test_transactions_unified.py::TestUnifiedRunCommand::test_run_command_fails_with_secondary_read_preference_from_transaction_options
```

## How this is generated

**pymongo's tests are run unmodified.** The submodule at `vendor/pymongo-tests/` is checked out at the pinned upstream tag with zero local edits — `git diff HEAD` inside the submodule is empty. The integration is entirely external: `pymongo_validation/plugin.py` starts an embedded Rust server (`_secantus_server.RustServer(storage_path=<fresh tempdir>, port=0)` — the in-process Rust accept loop over the pure-Rust engines and WiredTiger-backed storage; Python is only the launcher) (real on-disk WiredTiger via `tempfile.mkdtemp(prefix='secantus-pymongo-gauge-')`, not `:memory:`) in `pytest_configure` and writes the bound host/port into `DB_IP` + `DB_PORT` — the env vars pymongo's own `helpers_shared.py` reads at import time. Pytest then collects and runs the in-scope test paths defined in `pymongo_validation/include_paths.py`.

Tests gated on replica-set / sharding / auth / TLS / encryption topology self-skip — those skips are honest gaps, not failures. The pass rate above is therefore a meaningful conformance number: those are pymongo's actual tests, exercising SecantusDB the same way they exercise a real `mongod` in pymongo's CI.
