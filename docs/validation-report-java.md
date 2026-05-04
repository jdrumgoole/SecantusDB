# mongo-java-driver Validation Report

Generated 2026-05-04 — SecantusDB 0.2.0a18 vs mongo-java-driver cb45be6bb147 (`vendor/mongo-java-driver/`).

Run `uv run python -m invoke validate-java` to refresh. The pass rate is the analogue of the pymongo / mongo-go-driver / mongo-node-driver gauges for the official Java driver — the language enterprise MongoDB consumers most often use.

## Summary by module

| Module | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `bson` | 3809 | 0 | 11 | 3820 | 100.0% |
| **Overall** | **3809** | **0** | **11** | **3820** | **100.0%** |

## How this is generated

**mongo-java-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-java-driver/` is checked out at the pinned upstream tag with zero local edits. `java_validation/runner.py` finds a free port, spawns `python -m secantus --host 127.0.0.1 --port <free> --storage-path :memory:` as a subprocess, then invokes the driver's bundled gradle wrapper (`./gradlew --no-daemon -Dorg.mongodb.test.uri=mongodb://...`) for the in-scope modules in `java_validation/include_modules.py`. The system property is the seam Java's `ClusterFixture` test infrastructure reads (`org.mongodb.test.uri`); Gradle forwards it to the test JVM.

The driver writes JUnit XML to `<module>/build/test-results/test/TEST-*.xml`; we copy those out of the vendored tree (so the submodule stays untouched) and parse them here. Tests gated on a real `mongod` topology that SecantusDB doesn't aim to provide are out of scope and excluded from `include_modules.py` rather than letting them fail.
