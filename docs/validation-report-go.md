# mongo-go-driver Validation Report

Generated 2026-05-03 — SecantusDB 0.2.0a5 vs mongo-go-driver fd85a834c40e (`vendor/mongo-go-driver/`).

Run `uv run python -m invoke validate-go` to refresh. The pass rate is the analogue of the pymongo conformance gauge for the official Go driver — same shape, different wire-protocol pickiness. Type-strict bugs (int32 vs int64) that pymongo accepts silently fail loudly here.

## Summary by package

| Package | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `bson` | 2793 | 3 | 3 | 2799 | 99.9% |
| `mongo` | 275 | 9 | 2 | 286 | 96.8% |
| **Overall** | **3068** | **12** | **5** | **3085** | **99.6%** |

## Failures (12)

First 30 failed tests for triage:

```
bson :: TestBSONCorpus
bson :: TestBsonBinaryVectorSpec
bson :: FuzzDecode
mongo :: TestReadWriteConcernSpec/connstring
mongo :: TestReadWriteConcernSpec/document
mongo :: TestReadWriteConcernSpec
mongo :: TestConvenientTransactions/retry_timeout_enforced/unknown_transaction_commit_result
mongo :: TestConvenientTransactions/retry_timeout_enforced/commit_transient_transaction_error
mongo :: TestConvenientTransactions/retry_timeout_enforced
mongo :: TestConvenientTransactions/context_error_before_commitTransaction_does_not_retry_and_aborts
mongo :: TestConvenientTransactions/slow_operation_in_callback_retries
mongo :: TestConvenientTransactions
```

## How this is generated

**mongo-go-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-go-driver/` is checked out at the pinned upstream tag with zero local edits. `go_validation/runner.py` spawns `python -m secantus --host 127.0.0.1 --port <free> --storage-path :memory:` as a subprocess, waits for its TCP listener, exports `MONGODB_URI` (the env var `internal/integtest.MongoDBURI` and `internal/integration/mtest` read at setup), then runs `go test -json -count=1 <packages>` for the in-scope set in `go_validation/include_packages.py`. From the go-driver's point of view it's connecting to a real `mongod` over TCP — exactly like its CI does.

Tests gated on topology (`mtest.RequiresReplicaSet`, `mtest.RequiresSharded`, etc.) self-skip when the server doesn't match — those skips are honest gaps, not failures. Pass rate above is therefore a meaningful conformance number against the same language-canonical driver mongodump and mongorestore are built on.
