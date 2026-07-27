# mongo-java-driver Validation Report

Generated 2026-07-27 — SecantusDB 0.6.0b1 vs mongo-java-driver cb45be6bb147 (`vendor/mongo-java-driver/`).

Run `uv run python -m invoke validate-java` to refresh. The pass rate is the analogue of the pymongo / mongo-go-driver / mongo-node-driver gauges for the official Java driver — the language enterprise MongoDB consumers most often use.

## Scope

`driver-sync/src/test/functional/` contains **112** test classes upstream. The gauge currently runs **21** of them (~19%). The other 91 are either intentionally out of scope (encryption / atlas-search / kotlin-or-scala wrappers / OCSP / DNS / retryable / monitoring) or unaudited — they haven't been added to `java_validation/include_modules.py` because each new class needs the runner's wall-clock guard to confirm it terminates before it ships. The pass rate below describes the included subset, not the whole functional tree.

## Summary by module

| Module | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `driver-core__2` | 10 | 0 | 0 | 10 | 100.0% |
| `driver-sync__0` | 360 | 0 | 400 | 760 | 100.0% |
| `driver-sync__1` | 76 | 1 | 53 | 130 | 98.7% |
| **Overall** | **446** | **1** | **453** | **900** | **99.8%** |

## Failures (1)

First 30 failed tests for triage:

```
driver-sync__1 :: com.mongodb.client.ClientMetadataTest#client metadata is not propagated to the server: metadata append does not create new connections or close existing ones and no hello command is sent
```

## How this is generated

**mongo-java-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-java-driver/` is checked out at the pinned upstream tag with zero local edits. `java_validation/runner.py` does a two-phase spawn: phase 1 boots `python -m secantus --port 27018 --storage-path <tempdir> --standalone` without `--auth` and uses pymongo to createUser `root-user` (root role); phase 2 stops that daemon and restarts on the same tempdir **with `--auth`**, so the user record persists and the server now enforces auth. Gradle then runs the driver's bundled wrapper (`./gradlew --no-daemon -Dorg.mongodb.test.uri=mongodb://root-user:password@127.0.0.1:27018/?authSource=admin`) for the in-scope modules in `java_validation/include_modules.py`. The system property is the seam Java's `ClusterFixture` test infrastructure reads; Gradle forwards it to the test JVM. Standalone topology is critical: without `--standalone` the driver's `getSecondary()` is an unbounded sleep loop on non-RS deployments.

These are **integration specs** under `driver-sync/src/test/functional/` — every test opens a real TCP connection to the SecantusDB daemon, SCRAM-authenticates, and exchanges wire commands end-to-end. The pass rate is therefore a true measure of SecantusDB's compatibility with the Java driver, not of the driver's own pure-code logic.

The include set is currently narrow on purpose — `MongoCollectionTest`, `MongoClientTest`, `ExplainTest`, `ReadConcernTest`, `MongoWriteConcernWithResponseExceptionTest` — added one at a time as each is proven to terminate against SecantusDB. The driver writes JUnit XML to `<module>/build/test-results/test/TEST-*.xml`; we copy those out of the vendored tree (so the submodule stays untouched) and parse them here. Widen `include_modules.py` to add more test classes.
