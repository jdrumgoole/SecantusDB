# mongo-go-driver Validation Report

Generated 2026-05-10 — SecantusDB 0.4.0b16 vs mongo-go-driver fd85a834c40e (`vendor/mongo-go-driver/`).

Run `uv run python -m invoke validate-go` to refresh. The pass rate is the analogue of the pymongo conformance gauge for the official Go driver — same shape, different wire-protocol pickiness. Type-strict bugs (int32 vs int64) that pymongo accepts silently fail loudly here.

## Summary by package

| Package | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `internal/integration` | 245 | 2 | 25 | 272 | 99.2% |
| `internal/integration/unified` | 42 | 0 | 0 | 42 | 100.0% |
| **Overall** | **287** | **2** | **25** | **314** | **99.3%** |

## Failures (2)

First 30 failed tests for triage:

```
internal/integration :: TestChangeStream_ReplicaSet/resume_token_updated_on_empty_batch
internal/integration :: TestChangeStream_ReplicaSet
```

## How this is generated

**mongo-go-driver's integration tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-go-driver/` is checked out at the pinned upstream tag with zero local edits. `go_validation/runner.py` spawns `python -m secantus --host 127.0.0.1 --port 27018 --storage-path ':memory:' --noop-heartbeat-seconds 10` as a subprocess, waits for its TCP listener, exports `MONGODB_URI=mongodb://127.0.0.1:27018` (the env var `internal/integtest.MongoDBURI` and `internal/integration/mtest` read at setup), then runs `go test -json -count=1 ./internal/integration/...`. From the go-driver's point of view it's connecting to a real `mongod` over TCP — exactly like its CI does.

**Integration-only.** The pure-BSON unit tests under `./bson/...` and `./mongo` are out of scope for this gauge — they verify the driver's own serialization logic without ever opening a TCP connection, and would inflate the pass count without proving anything about SecantusDB's wire path. The pass rate above is a true measure of cross-driver compatibility with the language-canonical Go driver `mongodump` and `mongorestore` are built on.

Tests gated on topology (`mtest.RequiresReplicaSet`, `mtest.RequiresSharded`, etc.) self-skip when the server doesn't match — those skips are honest gaps, not failures.
