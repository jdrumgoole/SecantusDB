# mongo-java-driver Validation Report

Generated 2026-05-11 — SecantusDB 0.5.0b13 vs mongo-java-driver cb45be6bb147 (`vendor/mongo-java-driver/`).

Run `uv run python -m invoke validate-java` to refresh. The pass rate is the analogue of the pymongo / mongo-go-driver / mongo-node-driver gauges for the official Java driver — the language enterprise MongoDB consumers most often use.

## Summary by module

| Module | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `driver-sync` | 173 | 181 | 310 | 664 | 48.9% |
| **Overall** | **173** | **181** | **310** | **664** | **48.9%** |

## Failures (181)

First 30 failed tests for triage:

```
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#bulkWrite-updateMany-pipeline: UpdateMany in bulk write using pipelines
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#deleteMany-collation: DeleteMany when many documents match with collation
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#deleteOne-let: deleteOne with let option
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#updateOne-dots_and_dollars: Updating document to set top-level dollar-prefixed key on 5.0+ server
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#updateOne-dots_and_dollars: Updating document to set top-level dotted key on 5.0+ server
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#updateOne-dots_and_dollars: Updating document to set dollar-prefixed key in embedded doc on 5.0+ server
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#updateOne-dots_and_dollars: Updating document to set dotted key in embedded doc on 5.0+ server
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#insertMany-dots_and_dollars: Inserting document with top-level dollar-prefixed key on 5.0+ server
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#insertMany-dots_and_dollars: Inserting document with top-level dotted key
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#insertMany-dots_and_dollars: Inserting document with dollar-prefixed key in embedded doc
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#insertMany-dots_and_dollars: Inserting document with dotted key in embedded doc
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndReplace-hint: FindOneAndReplace with hint string
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndReplace-hint: FindOneAndReplace with hint document
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndUpdate-pipeline: FindOneAndUpdate using pipelines
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndUpdate-arrayFilters: FindOneAndUpdate when no document matches arrayFilters
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndUpdate-arrayFilters: FindOneAndUpdate when one document matches arrayFilters
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndUpdate-arrayFilters: FindOneAndUpdate when multiple documents match arrayFilters
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#aggregate-out: Aggregate with $out
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#insertOne-comment: insertOne with string comment
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#insertOne-comment: insertOne with document comment
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#updateMany-let: updateMany with let option
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndDelete-collation: FindOneAndDelete when one document matches with collation
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndUpdate-errorResponse: findOneAndUpdate DuplicateKey error is accessible
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndUpdate-errorResponse: findOneAndUpdate document validation errInfo is accessible
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#find-let: Find with let option
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndReplace-upsert: FindOneAndReplace when no documents match without id specified with upsert returning the document before modification
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndReplace-upsert: FindOneAndReplace when no documents match without id specified with upsert returning the document after modification
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndReplace-upsert: FindOneAndReplace when no documents match with id specified with upsert returning the document before modification
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#findOneAndReplace-upsert: FindOneAndReplace when no documents match with id specified with upsert returning the document after modification
driver-sync :: com.mongodb.client.unified.UnifiedCrudTest#deleteMany-comment: deleteMany with string comment
```
... and 151 more (see raw XML).

## How this is generated

**mongo-java-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-java-driver/` is checked out at the pinned upstream tag with zero local edits. `java_validation/runner.py` does a two-phase spawn: phase 1 boots `python -m secantus --port 27018 --storage-path <tempdir> --standalone` without `--auth` and uses pymongo to createUser `root-user` (root role); phase 2 stops that daemon and restarts on the same tempdir **with `--auth`**, so the user record persists and the server now enforces auth. Gradle then runs the driver's bundled wrapper (`./gradlew --no-daemon -Dorg.mongodb.test.uri=mongodb://root-user:password@127.0.0.1:27018/?authSource=admin`) for the in-scope modules in `java_validation/include_modules.py`. The system property is the seam Java's `ClusterFixture` test infrastructure reads; Gradle forwards it to the test JVM. Standalone topology is critical: without `--standalone` the driver's `getSecondary()` is an unbounded sleep loop on non-RS deployments.

These are **integration specs** under `driver-sync/src/test/functional/` — every test opens a real TCP connection to the SecantusDB daemon, SCRAM-authenticates, and exchanges wire commands end-to-end. The pass rate is therefore a true measure of SecantusDB's compatibility with the Java driver, not of the driver's own pure-code logic.

The include set is currently narrow on purpose — `MongoCollectionTest`, `MongoClientTest`, `ExplainTest`, `ReadConcernTest`, `MongoWriteConcernWithResponseExceptionTest` — added one at a time as each is proven to terminate against SecantusDB. The driver writes JUnit XML to `<module>/build/test-results/test/TEST-*.xml`; we copy those out of the vendored tree (so the submodule stays untouched) and parse them here. Widen `include_modules.py` to add more test classes.
