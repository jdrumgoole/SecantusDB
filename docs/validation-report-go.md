# mongo-go-driver Validation Report

Generated 2026-05-07 — SecantusDB 0.3.0a66 vs mongo-go-driver fd85a834c40e (`vendor/mongo-go-driver/`).

Run `uv run python -m invoke validate-go` to refresh. The pass rate is the analogue of the pymongo conformance gauge for the official Go driver — same shape, different wire-protocol pickiness. Type-strict bugs (int32 vs int64) that pymongo accepts silently fail loudly here.

## Summary by package

| Package | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `bson` | 5061 | 0 | 14 | 5075 | 100.0% |
| `internal/integration` | 270 | 15 | 30 | 315 | 94.7% |
| `internal/integration/unified` | 42 | 0 | 0 | 42 | 100.0% |
| `mongo` | 303 | 0 | 10 | 313 | 100.0% |
| **Overall** | **5676** | **15** | **54** | **5745** | **99.7%** |

## Failures (15)

First 30 failed tests for triage:

```
internal/integration :: TestChangeStream_ReplicaSet/resume_token_updated_on_empty_batch
internal/integration :: TestChangeStream_ReplicaSet
internal/integration :: TestCursor_All/getMore_error
internal/integration :: TestCursor_All
internal/integration :: TestCursor_Close/killCursors_error
internal/integration :: TestCursor_Close
internal/integration :: TestDatabase/run_command/gets_result_and_error
internal/integration :: TestDatabase/run_command
internal/integration :: TestDatabase/list_collection_names/filter_not_found
internal/integration :: TestDatabase/list_collection_names
internal/integration :: TestDatabase/list_collections/verify_results/replica_set_filter
internal/integration :: TestDatabase/list_collections/verify_results
internal/integration :: TestDatabase/list_collections/getMore_commands_are_monitored
internal/integration :: TestDatabase/list_collections
internal/integration :: TestDatabase
```

## How this is generated

**mongo-go-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-go-driver/` is checked out at the pinned upstream tag with zero local edits. A nested submodule `testdata/specifications/` (the MongoDB driver-spec JSON corpus) is also pulled — without it the bson-corpus tests fail on missing files. `go_validation/runner.py` spawns `python -m secantus --host 127.0.0.1 --port <free> --storage-path ':memory:'` as a subprocess, waits for its TCP listener, exports `MONGODB_URI` (the env var `internal/integtest.MongoDBURI` and `internal/integration/mtest` read at setup), then runs `go test -json -count=1 <packages>` for the in-scope set in `go_validation/include_packages.py`. From the go-driver's point of view it's connecting to a real `mongod` over TCP — exactly like its CI does.

Tests gated on topology (`mtest.RequiresReplicaSet`, `mtest.RequiresSharded`, etc.) self-skip when the server doesn't match — those skips are honest gaps, not failures. Pass rate above is therefore a meaningful conformance number against the same language-canonical driver mongodump and mongorestore are built on.
