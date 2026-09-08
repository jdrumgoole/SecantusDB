# mongo-csharp-driver Validation Report

Generated 2026-08-31 — SecantusDB 0.6.0b16 vs mongo-csharp-driver 8297e62 (`vendor/mongo-csharp-driver/`).

Run `uv run python -m invoke validate-dotnet` to refresh. The official MongoDB **C# / .NET** driver — its xUnit integration suite (`MongoDB.Driver.Tests`) run unmodified against an embedded SecantusDB daemon via `dotnet test`.

## Summary by namespace

| Namespace | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `MongoDB.Driver.Tests.Specifications.crud` | 196 | 0 | 0 | 196 | 100.0% |
| `MongoDB.Driver.Tests.Specifications.crud.prose_tests` | 13 | 19 | 0 | 32 | 40.6% |
| **Overall** | **209** | **19** | **0** | **228** | **91.7%** |

## Failures (19)

First 30 failed tests for triage:

```
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_unacknowledged_write_concern_uses_w0_all_batches(async: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_splits_batches_on_maxMessageSizeBytes(async: False)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_handles_getMore_error(async: False)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_handles_individual_WriteError_across_batches(async: False, ordered: False)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_handles_individual_WriteError_across_batches(async: True, ordered: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_handles_cursor_requiring_getMore(async: True, isInTransaction: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.Ensure_generated_ids_are_first_fields_in_document_using_client_bulkWrite(async: False)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_collects_WriteConcernError_across_batches(async: False)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_handles_getMore_error(async: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_splits_batches_on_maxMessageSizeBytes(async: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_handles_individual_WriteError_across_batches(async: False, ordered: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_collects_WriteConcernError_across_batches(async: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_handles_individual_WriteError_across_batches(async: True, ordered: False)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.Ensure_generated_ids_are_first_fields_in_document_using_client_bulkWrite(async: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_splits_batches_on_maxWriteBatchSize(async: False)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_handles_cursor_requiring_getMore(async: False, isInTransaction: False)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_splits_batches_on_maxWriteBatchSize(async: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_handles_cursor_requiring_getMore(async: False, isInTransaction: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_handles_cursor_requiring_getMore(async: True, isInTransaction: False)
```

## How this is generated

`invoke validate-dotnet` spawns a SecantusDB daemon on a fresh ephemeral port and runs the mongo-csharp-driver xUnit integration project via `dotnet test` with `MONGODB_URI` pointed at the daemon and Catch2-style out-of-scope categories excluded via `--filter` (see `dotnet_validation/include_paths.py`), writing TRX results that this script renders.
