# pymongo Validation Report

Generated 2026-05-09 — SecantusDB 0.3.0a76 vs pymongo f2103a95870a (`vendor/pymongo-tests/`).

Run `uv run python -m invoke validate` to refresh. The pass rate is the best honest measure of how close SecantusDB is to a complete MongoDB surrogate for the in-scope wire-protocol surface; gaps are the to-do list.

## Summary by category

| Category | Passed | Failed | Errored | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|---:|
| `test_binary.py` | 29 | 0 | 0 | 0 | 29 | 100.0% |
| `test_bson.py` | 87 | 0 | 0 | 1 | 88 | 100.0% |
| `test_bson_corpus.py` | 31 | 0 | 0 | 0 | 31 | 100.0% |
| `test_bulk.py` | 34 | 0 | 0 | 4 | 38 | 100.0% |
| `test_change_stream.py` | 1 | 0 | 0 | 154 | 155 | 100.0% |
| `test_code.py` | 8 | 0 | 0 | 0 | 8 | 100.0% |
| `test_collection.py` | 86 | 0 | 0 | 5 | 91 | 100.0% |
| `test_collection_management.py` | 7 | 0 | 0 | 0 | 7 | 100.0% |
| `test_command_monitoring.py` | 32 | 0 | 0 | 6 | 38 | 100.0% |
| `test_comment.py` | 1 | 0 | 0 | 2 | 3 | 100.0% |
| `test_common.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_crud_unified.py` | 336 | 0 | 0 | 150 | 486 | 100.0% |
| `test_crud_v1.py` | 14 | 0 | 0 | 0 | 14 | 100.0% |
| `test_cursor.py` | 59 | 0 | 0 | 13 | 72 | 100.0% |
| `test_custom_types.py` | 39 | 0 | 0 | 12 | 51 | 100.0% |
| `test_database.py` | 34 | 0 | 0 | 2 | 36 | 100.0% |
| `test_dbref.py` | 12 | 0 | 0 | 0 | 12 | 100.0% |
| `test_decimal128.py` | 4 | 0 | 0 | 0 | 4 | 100.0% |
| `test_default_exports.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| `test_errors.py` | 8 | 0 | 0 | 0 | 8 | 100.0% |
| `test_examples.py` | 14 | 0 | 0 | 6 | 20 | 100.0% |
| `test_json_util.py` | 24 | 0 | 0 | 0 | 24 | 100.0% |
| `test_objectid.py` | 15 | 0 | 0 | 0 | 15 | 100.0% |
| `test_operations.py` | 2 | 0 | 0 | 0 | 2 | 100.0% |
| `test_raw_bson.py` | 14 | 0 | 0 | 0 | 14 | 100.0% |
| `test_read_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| `test_read_preferences.py` | 9 | 0 | 0 | 20 | 29 | 100.0% |
| `test_results.py` | 5 | 0 | 0 | 0 | 5 | 100.0% |
| `test_run_command.py` | 14 | 0 | 0 | 7 | 21 | 100.0% |
| `test_son.py` | 11 | 0 | 0 | 0 | 11 | 100.0% |
| `test_timestamp.py` | 7 | 0 | 0 | 0 | 7 | 100.0% |
| `test_write_concern.py` | 6 | 0 | 0 | 0 | 6 | 100.0% |
| **Overall** | **959** | **0** | **0** | **382** | **1341** | **100.0%** |

## How this is generated

**pymongo's tests are run unmodified.** The submodule at `vendor/pymongo-tests/` is checked out at the pinned upstream tag with zero local edits — `git diff HEAD` inside the submodule is empty. The integration is entirely external: `pymongo_validation/plugin.py` starts an embedded `SecantusDBServer(host='127.0.0.1', port=0, storage_path=':memory:')` in `pytest_configure` and writes the bound host/port into `DB_IP` + `DB_PORT` — the env vars pymongo's own `helpers_shared.py` reads at import time. Pytest then collects and runs the in-scope test paths defined in `pymongo_validation/include_paths.py`.

Tests gated on replica-set / sharding / auth / TLS / encryption topology self-skip — those skips are honest gaps, not failures. The pass rate above is therefore a meaningful conformance number: those are pymongo's actual tests, exercising SecantusDB the same way they exercise a real `mongod` in pymongo's CI.
