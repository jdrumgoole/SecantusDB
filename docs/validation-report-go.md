# mongo-go-driver Validation Report

Generated 2026-05-06 — SecantusDB 0.3.0a63 vs mongo-go-driver fd85a834c40e (`vendor/mongo-go-driver/`).

Run `uv run python -m invoke validate-go` to refresh. The pass rate is the analogue of the pymongo conformance gauge for the official Go driver — same shape, different wire-protocol pickiness. Type-strict bugs (int32 vs int64) that pymongo accepts silently fail loudly here.

## Summary by package

| Package | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `bson` | 5061 | 0 | 14 | 5075 | 100.0% |
| `internal/integration` | 207 | 8 | 22 | 237 | 96.3% |
| `internal/integration/unified` | 42 | 0 | 0 | 42 | 100.0% |
| `mongo` | 303 | 0 | 10 | 313 | 100.0% |
| **Overall** | **5613** | **8** | **46** | **5667** | **99.9%** |

## Failures (8)

First 30 failed tests for triage:

```
internal/integration :: TestCollection/find/invalid_identifier_error
internal/integration :: TestCollection/find
internal/integration :: TestCollection/insert_many/return_only_inserted_ids/ordered
internal/integration :: TestCollection/insert_many/large_document_batches
internal/integration :: TestCollection/insert_many/return_only_inserted_ids
internal/integration :: TestCollection/insert_many/writeError_index
internal/integration :: TestCollection/insert_many
internal/integration :: TestCollection
```

## How this is generated

**mongo-go-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-go-driver/` is checked out at the pinned upstream tag with zero local edits. A nested submodule `testdata/specifications/` (the MongoDB driver-spec JSON corpus) is also pulled — without it the bson-corpus tests fail on missing files. `go_validation/runner.py` spawns `python -m secantus --host 127.0.0.1 --port <free> --storage-path ':memory:'` as a subprocess, waits for its TCP listener, exports `MONGODB_URI` (the env var `internal/integtest.MongoDBURI` and `internal/integration/mtest` read at setup), then runs `go test -json -count=1 <packages>` for the in-scope set in `go_validation/include_packages.py`. From the go-driver's point of view it's connecting to a real `mongod` over TCP — exactly like its CI does.

Tests gated on topology (`mtest.RequiresReplicaSet`, `mtest.RequiresSharded`, etc.) self-skip when the server doesn't match — those skips are honest gaps, not failures. Pass rate above is therefore a meaningful conformance number against the same language-canonical driver mongodump and mongorestore are built on.
