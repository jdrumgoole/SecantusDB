# mongo-csharp-driver Validation Report

Generated 2026-06-19 — SecantusDB 0.5.4b7 vs mongo-csharp-driver v3.9.0 (`vendor/mongo-csharp-driver/`).

Run `uv run python -m invoke validate-dotnet` to refresh. The official MongoDB **C# / .NET** driver — its xUnit integration suite (`MongoDB.Driver.Tests`) run unmodified against an embedded SecantusDB daemon via `dotnet test`.

## Summary by namespace

| Namespace | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `MongoDB.Driver.Tests.Specifications.crud` | 196 | 0 | 0 | 196 | 100.0% |
| `MongoDB.Driver.Tests.Specifications.crud.prose_tests` | 5 | 1 | 26 | 32 | 83.3% |
| **Overall** | **201** | **1** | **26** | **228** | **99.5%** |

## Failures (1)

First 30 failed tests for triage:

```
MongoDB.Driver.Tests.Specifications.crud.prose_tests.CrudProseTests.WriteError_details_should_expose_writeErrors_errInfo
```

## How this is generated

`invoke validate-dotnet` spawns a SecantusDB daemon on a fresh ephemeral port and runs the mongo-csharp-driver xUnit integration project via `dotnet test` with `MONGODB_URI` pointed at the daemon and Catch2-style out-of-scope categories excluded via `--filter` (see `dotnet_validation/include_paths.py`), writing TRX results that this script renders.
