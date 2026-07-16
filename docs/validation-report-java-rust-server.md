# mongo-java-driver Validation Report (Rust server)

Generated 2026-07-16 — SecantusDB 0.5.4b234 vs mongo-java-driver cb45be6bb147 (`vendor/mongo-java-driver/`).

Run `uv run python -m invoke validate-java --server rust` to refresh. The same unmodified suite as `docs/validation-report-java.md`, pointed at the standalone **Rust server** (`secantusd-rs`) instead of the Python one — the gap between the two reports is part of the Rust server's remaining to-do list.

## Scope

`driver-sync/src/test/functional/` contains **112** test classes upstream. The gauge currently runs **21** of them (~19%). The other 91 are either intentionally out of scope (encryption / atlas-search / kotlin-or-scala wrappers / OCSP / DNS / retryable / monitoring) or unaudited — they haven't been added to `java_validation/include_modules.py` because each new class needs the runner's wall-clock guard to confirm it terminates before it ships. The pass rate below describes the included subset, not the whole functional tree.

## Summary by module

| Module | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `driver-core__2` | 9 | 1 | 0 | 10 | 90.0% |
| `driver-sync__0` | 358 | 2 | 400 | 760 | 99.4% |
| `driver-sync__1` | 77 | 0 | 53 | 130 | 100.0% |
| **Overall** | **444** | **3** | **453** | **900** | **99.3%** |

## Failures (3)

First 30 failed tests for triage:

```
driver-core__2 :: com.mongodb.client.model.GeoFiltersFunctionalSpecification#$geoWithin $center
driver-sync__0 :: com.mongodb.client.MongoCollectionTest#testMapReduceWithGenerics()
driver-sync__0 :: com.mongodb.client.unified.UnifiedWriteConcernTest#default-write-concern-3.4: MapReduce omits default write concern
```

## How this is generated

**mongo-java-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-java-driver/` is checked out at the pinned upstream tag with zero local edits. `java_validation/runner.py` does a two-phase spawn: phase 1 boots `python -m secantus --port 27018 --storage-path <tempdir> --standalone` without `--auth` and uses pymongo to createUser `root-user` (root role); phase 2 stops that daemon and restarts on the same tempdir **with `--auth`**, so the user record persists and the server now enforces auth. Gradle then runs the driver's bundled wrapper (`./gradlew --no-daemon -Dorg.mongodb.test.uri=mongodb://root-user:password@127.0.0.1:27018/?authSource=admin`) for the in-scope modules in `java_validation/include_modules.py`. The system property is the seam Java's `ClusterFixture` test infrastructure reads; Gradle forwards it to the test JVM. Standalone topology is critical: without `--standalone` the driver's `getSecondary()` is an unbounded sleep loop on non-RS deployments.

In `--server rust` mode the same two-phase spawn runs the standalone `secantusd-rs` binary (via `gauge_common.for_server`, same flags) instead of `python -m secantus`, so the numbers above measure the Rust server.

These are **integration specs** under `driver-sync/src/test/functional/` — every test opens a real TCP connection to the SecantusDB daemon, SCRAM-authenticates, and exchanges wire commands end-to-end. The pass rate is therefore a true measure of SecantusDB's compatibility with the Java driver, not of the driver's own pure-code logic.

The include set is currently narrow on purpose — `MongoCollectionTest`, `MongoClientTest`, `ExplainTest`, `ReadConcernTest`, `MongoWriteConcernWithResponseExceptionTest` — added one at a time as each is proven to terminate against SecantusDB. The driver writes JUnit XML to `<module>/build/test-results/test/TEST-*.xml`; we copy those out of the vendored tree (so the submodule stays untouched) and parse them here. Widen `include_modules.py` to add more test classes.
