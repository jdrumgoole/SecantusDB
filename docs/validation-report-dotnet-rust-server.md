# mongo-csharp-driver Validation Report

Measured 2026-11-02 (raw artifact), generated 2026-11-02 — SecantusDB 0.7.0b3 vs mongo-csharp-driver 8297e62 (`vendor/mongo-csharp-driver/`).

Run `uv run python -m invoke validate-dotnet` to refresh. The official MongoDB **C# / .NET** driver — its xUnit integration suite (`MongoDB.Driver.Tests`) run unmodified against an embedded SecantusDB daemon via `dotnet test`.

## Summary by namespace

| Namespace | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `MongoDB.Driver.Tests.Specifications.crud` | 196 | 0 | 0 | 196 | 100.0% |
| `MongoDB.Driver.Tests.Specifications.crud.prose_tests` | 30 | 2 | 0 | 32 | 93.7% |
| **Overall** | **226** | **2** | **0** | **228** | **99.1%** |

## Failures (2)

First 30 failed tests for triage:

```
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_collects_WriteConcernError_across_batches(async: True)
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.MongoClient_bulkWrite_collects_WriteConcernError_across_batches(async: False)
```

## How this is generated

`invoke validate-dotnet` spawns a SecantusDB daemon on a fresh ephemeral port and runs the mongo-csharp-driver xUnit integration project via `dotnet test` with `MONGODB_URI` pointed at the daemon and Catch2-style out-of-scope categories excluded via `--filter` (see `dotnet_validation/include_paths.py`), writing TRX results that this script renders.
