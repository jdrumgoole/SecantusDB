# mongo-go-driver Validation Report

Generated 2026-10-05 — SecantusDB 0.6.0b17 vs mongo-go-driver fd85a834c40e (`vendor/mongo-go-driver/`).

Run `uv run python -m invoke validate-go` to refresh. The pass rate is the analogue of the pymongo conformance gauge for the official Go driver — same shape, different wire-protocol pickiness. Type-strict bugs (int32 vs int64) that pymongo accepts silently fail loudly here.

## Summary by package

| Package | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `internal/integration` | 554 | 14 | 49 | 617 | 97.5% |
| `internal/integration/unified` | 42 | 0 | 0 | 42 | 100.0% |
| **Overall** | **596** | **14** | **49** | **659** | **97.7%** |

## Failures (14)

First 30 failed tests for triage:

```
internal/integration :: TestRetryableReadsProse/retrying_reads_in_a_replica_set/overload_errors_retried_on_a_different_replicaset_server
internal/integration :: TestRetryableReadsProse/retrying_reads_in_a_replica_set
internal/integration :: TestRetryableReadsProse
internal/integration :: TestSDAMErrorHandling/after_handshake_completes/network_errors/pool_not_cleared_on_timeout_network_error
internal/integration :: TestSDAMErrorHandling/after_handshake_completes/network_errors/pool_not_cleared_on_context_cancellation
internal/integration :: TestSDAMErrorHandling/after_handshake_completes/network_errors
internal/integration :: TestSDAMErrorHandling/after_handshake_completes
internal/integration :: TestSDAMErrorHandling
internal/integration :: TestSDAMProse/heartbeats_processed_more_frequently
internal/integration :: TestSDAMProse
internal/integration :: TestConnectionPoolBackpressure
internal/integration :: TestSessionsMongocryptdProse/18._implicit_session_is_ignored_if_connection_does_not_support_sessions
internal/integration :: TestSessionsMongocryptdProse/19._explicit_session_raises_an_error_if_connection_does_not_support_sessions
internal/integration :: TestSessionsMongocryptdProse
```

## How this is generated

**mongo-go-driver's integration tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-go-driver/` is checked out at the pinned upstream tag with zero local edits. `go_validation/runner.py` spawns `python -m secantus --host 127.0.0.1 --port 27018 --storage-path <tempdir> --noop-heartbeat-seconds 10` as a subprocess (a fresh `tempfile.mkdtemp(prefix='secantus-go-gauge-')` — never `:memory:`; on-disk WiredTiger keeps the checkpoint / journal code paths exercised), waits for its TCP listener, exports `MONGODB_URI=mongodb://127.0.0.1:27018` (the env var `internal/integtest.MongoDBURI` and `internal/integration/mtest` read at setup), then runs `go test -json -count=1 ./internal/integration/...`. From the go-driver's point of view it's connecting to a real `mongod` over TCP — exactly like its CI does.

**Integration-only.** The pure-BSON unit tests under `./bson/...` and `./mongo` are out of scope for this gauge — they verify the driver's own serialization logic without ever opening a TCP connection, and would inflate the pass count without proving anything about SecantusDB's wire path. The pass rate above is a true measure of cross-driver compatibility with the language-canonical Go driver `mongodump` and `mongorestore` are built on.

Tests gated on topology (`mtest.RequiresReplicaSet`, `mtest.RequiresSharded`, etc.) self-skip when the server doesn't match — those skips are honest gaps, not failures.
