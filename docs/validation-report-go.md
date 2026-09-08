# mongo-go-driver Validation Report

Generated 2026-08-31 — SecantusDB 0.6.0b16 vs mongo-go-driver fd85a834c40e (`vendor/mongo-go-driver/`).

Run `uv run python -m invoke validate-go` to refresh. The pass rate is the analogue of the pymongo conformance gauge for the official Go driver — same shape, different wire-protocol pickiness. Type-strict bugs (int32 vs int64) that pymongo accepts silently fail loudly here.

## Summary by package

| Package | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `internal/integration` | 367 | 30 | 37 | 434 | 92.4% |
| `internal/integration/unified` | 42 | 0 | 0 | 42 | 100.0% |
| **Overall** | **409** | **30** | **37** | **476** | **93.2%** |

## Failures (30)

First 30 failed tests for triage:

```
internal/integration :: TestClient_BulkWrite/bulk_write_with_write_concern/acknowledged
internal/integration :: TestClient_BulkWrite/bulk_write_with_write_concern
internal/integration :: TestClient_BulkWrite/bulk_write_with_large_messages
internal/integration :: TestClient_BulkWrite
internal/integration :: TestClient_BulkWrite_AddCommandFields/update_many_empty
internal/integration :: TestClient_BulkWrite_AddCommandFields/update_many_false
internal/integration :: TestClient_BulkWrite_AddCommandFields/insert_one_empty
internal/integration :: TestClient_BulkWrite_AddCommandFields/update_one_true
internal/integration :: TestClient_BulkWrite_AddCommandFields/insert_one_false
internal/integration :: TestClient_BulkWrite_AddCommandFields/insert_one_true
internal/integration :: TestClient_BulkWrite_AddCommandFields/update_one_empty
internal/integration :: TestClient_BulkWrite_AddCommandFields/update_one_false
internal/integration :: TestClient_BulkWrite_AddCommandFields/replace_one_false
internal/integration :: TestClient_BulkWrite_AddCommandFields/replace_one_true
internal/integration :: TestClient_BulkWrite_AddCommandFields/update_many_true
internal/integration :: TestClient_BulkWrite_AddCommandFields/replace_one_empty
internal/integration :: TestClient_BulkWrite_AddCommandFields
internal/integration :: TestClientBulkWriteProse/3._MongoClient.bulkWrite_batch_splits_a_writeModels_input_with_greater_than_maxWriteBatchSize_operations
internal/integration :: TestClientBulkWriteProse/4._MongoClient.bulkWrite_batch_splits_when_an_ops_payload_exceeds_maxMessageSizeBytes
internal/integration :: TestClientBulkWriteProse/6._MongoClient.bulkWrite_handles_individual_WriteErrors_across_batches/unordered
internal/integration :: TestClientBulkWriteProse/6._MongoClient.bulkWrite_handles_individual_WriteErrors_across_batches/ordered
internal/integration :: TestClientBulkWriteProse/6._MongoClient.bulkWrite_handles_individual_WriteErrors_across_batches
internal/integration :: TestClientBulkWriteProse/7._MongoClient.bulkWrite_handles_a_cursor_requiring_a_getMore
internal/integration :: TestClientBulkWriteProse/8._MongoClient.bulkWrite_handles_a_cursor_requiring_getMore_within_a_transaction
internal/integration :: TestClientBulkWriteProse/9._MongoClient.bulkWrite_handles_a_getMore_error
internal/integration :: TestClientBulkWriteProse/11._MongoClient.bulkWrite_batch_splits_when_the_addition_of_a_new_namespace_exceeds_the_maximum_message_size/Case_1:_No_batch-splitting_required
internal/integration :: TestClientBulkWriteProse/11._MongoClient.bulkWrite_batch_splits_when_the_addition_of_a_new_namespace_exceeds_the_maximum_message_size/Case_2:_Batch-splitting_required
internal/integration :: TestClientBulkWriteProse/11._MongoClient.bulkWrite_batch_splits_when_the_addition_of_a_new_namespace_exceeds_the_maximum_message_size
internal/integration :: TestClientBulkWriteProse/15._MongoClient.bulkWrite_with_unacknowledged_write_concern_uses_w:0_for_all_batches
internal/integration :: TestClientBulkWriteProse
```

## How this is generated

**mongo-go-driver's integration tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-go-driver/` is checked out at the pinned upstream tag with zero local edits. `go_validation/runner.py` spawns `python -m secantus --host 127.0.0.1 --port 27018 --storage-path <tempdir> --noop-heartbeat-seconds 10` as a subprocess (a fresh `tempfile.mkdtemp(prefix='secantus-go-gauge-')` — never `:memory:`; on-disk WiredTiger keeps the checkpoint / journal code paths exercised), waits for its TCP listener, exports `MONGODB_URI=mongodb://127.0.0.1:27018` (the env var `internal/integtest.MongoDBURI` and `internal/integration/mtest` read at setup), then runs `go test -json -count=1 ./internal/integration/...`. From the go-driver's point of view it's connecting to a real `mongod` over TCP — exactly like its CI does.

**Integration-only.** The pure-BSON unit tests under `./bson/...` and `./mongo` are out of scope for this gauge — they verify the driver's own serialization logic without ever opening a TCP connection, and would inflate the pass count without proving anything about SecantusDB's wire path. The pass rate above is a true measure of cross-driver compatibility with the language-canonical Go driver `mongodump` and `mongorestore` are built on.

Tests gated on topology (`mtest.RequiresReplicaSet`, `mtest.RequiresSharded`, etc.) self-skip when the server doesn't match — those skips are honest gaps, not failures.
