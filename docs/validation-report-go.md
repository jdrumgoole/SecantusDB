# mongo-go-driver Validation Report

Generated 2026-05-11 — SecantusDB 0.5.0b12 vs mongo-go-driver fd85a834c40e (`vendor/mongo-go-driver/`).

Run `uv run python -m invoke validate-go` to refresh. The pass rate is the analogue of the pymongo conformance gauge for the official Go driver — same shape, different wire-protocol pickiness. Type-strict bugs (int32 vs int64) that pymongo accepts silently fail loudly here.

## Summary by package

| Package | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `internal/integration` | 331 | 62 | 33 | 426 | 84.2% |
| `internal/integration/unified` | 42 | 0 | 0 | 42 | 100.0% |
| **Overall** | **373** | **62** | **33** | **468** | **85.7%** |

## Failures (62)

First 30 failed tests for triage:

```
internal/integration :: TestDatabase/list_collection_specifications/filter_passed_to_listCollections
internal/integration :: TestDatabase/list_collection_specifications
internal/integration :: TestDatabase/create_collection/options/all_options_except_collation_and_csppi
internal/integration :: TestDatabase/create_collection/options/changeStreamPreAndPostImages
internal/integration :: TestDatabase/create_collection/options
internal/integration :: TestDatabase/create_collection
internal/integration :: TestDatabase/create_view/function_parameters_are_translated_into_options
internal/integration :: TestDatabase/create_view
internal/integration :: TestDatabase
internal/integration :: TestErrors
internal/integration :: TestGridFS/download/error_if_files_collection_document_does_not_have_a_chunkSize_field
internal/integration :: TestGridFS/download/cursor_error_during_read_after_downloading
internal/integration :: TestGridFS/download/cursor_error_during_skip_after_downloading
internal/integration :: TestGridFS/download
internal/integration :: TestGridFS/bucket_collection_accessors/default_bucket_name
internal/integration :: TestGridFS/bucket_collection_accessors/custom_bucket_name
internal/integration :: TestGridFS/bucket_collection_accessors
internal/integration :: TestGridFS/Find
internal/integration :: TestGridFS
internal/integration :: TestHandshakeProse/1._valid_AWS
internal/integration :: TestHandshakeProse/2._valid_Azure
internal/integration :: TestHandshakeProse/3._valid_GCP
internal/integration :: TestHandshakeProse/4._valid_Vercel
internal/integration :: TestHandshakeProse/5._invalid_multiple_providers
internal/integration :: TestHandshakeProse/6._invalid_long_string
internal/integration :: TestHandshakeProse/7._invalid_wrong_types
internal/integration :: TestHandshakeProse/8._Invalid_-_AWS_EXECUTION_ENV_does_not_start_with_"AWS_Lambda_"
internal/integration :: TestHandshakeProse/driver_info_included
internal/integration :: TestHandshakeProse
internal/integration :: TestLoadBalancedConnectionHandshake/non-LB_connection_handshake_uses_OP_QUERY
```
... and 32 more (see raw NDJSON).

## How this is generated

**mongo-go-driver's integration tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-go-driver/` is checked out at the pinned upstream tag with zero local edits. `go_validation/runner.py` spawns `python -m secantus --host 127.0.0.1 --port 27018 --storage-path ':memory:' --noop-heartbeat-seconds 10` as a subprocess, waits for its TCP listener, exports `MONGODB_URI=mongodb://127.0.0.1:27018` (the env var `internal/integtest.MongoDBURI` and `internal/integration/mtest` read at setup), then runs `go test -json -count=1 ./internal/integration/...`. From the go-driver's point of view it's connecting to a real `mongod` over TCP — exactly like its CI does.

**Integration-only.** The pure-BSON unit tests under `./bson/...` and `./mongo` are out of scope for this gauge — they verify the driver's own serialization logic without ever opening a TCP connection, and would inflate the pass count without proving anything about SecantusDB's wire path. The pass rate above is a true measure of cross-driver compatibility with the language-canonical Go driver `mongodump` and `mongorestore` are built on.

Tests gated on topology (`mtest.RequiresReplicaSet`, `mtest.RequiresSharded`, etc.) self-skip when the server doesn't match — those skips are honest gaps, not failures.
