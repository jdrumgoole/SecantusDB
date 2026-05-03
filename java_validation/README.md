# java-validation

Runs **(a curated subset of) mongo-java-driver's own tests, unmodified**,
against an embedded `SecantusDBServer` and emits a markdown report at
`docs/validation-report-java.md`. The pass rate is the analogue of the
pymongo / mongo-go-driver / mongo-node-driver gauges for the official
Java driver — the language enterprise MongoDB consumers most often
use.

The submodule at `vendor/mongo-java-driver/` is checked out at the
pinned upstream tag (`r5.7.0`) with zero local edits. The integration
is entirely external.

## How to run

```bash
git submodule update --init --recursive   # pulls vendor/mongo-java-driver
uv run python -m invoke validate-java     # downloads gradle + runs + report
```

Requires a JDK >=8 on `PATH` (mongo-java-driver targets Java 8). On
macOS: `brew install openjdk@21` (or any LTS). On Linux: `apt install
default-jdk` / `dnf install java-21-openjdk-devel`.

The first run is slow because Gradle has to download its distribution
(~150 MB) and the driver's dependencies. Subsequent runs reuse the
caches under `~/.gradle/`.

## Initial scope

This baseline runs `:bson:test` only — pure unit tests of the driver's
BSON serialization library (~289 test files). They're fast (a few
minutes including the gradle warm-up), don't need a real-mongod
topology, and catch BSON-format regressions reliably.

The `driver-core`, `driver-sync`, and `driver-reactive-streams`
modules have substantially more coverage but their integration tests
expect a real mongod topology — replica-set primary advertisements,
change-stream cursors, multi-document transactions with rollback —
that SecantusDB intentionally doesn't provide. Adding `:driver-sync:test`
or specific test classes to `include_modules.py` is the way to widen.

## What's vendored

`vendor/mongo-java-driver/` is a git submodule pinned to a stable
mongo-java-driver release. Apache-2.0 licensed; we do not redistribute
— `pyproject.toml`'s `sdist.exclude` keeps it out of the wheel and
sdist. To bump:

```bash
cd vendor/mongo-java-driver && git checkout rX.Y.Z && cd ../..
git add vendor/mongo-java-driver
uv run python -m invoke validate-java        # refresh report
git commit -am "Bump java-validation target -> rX.Y.Z"
```

## How the integration works

1. `java_validation/runner.py` finds a free TCP port and spawns
   SecantusDB **as a standalone daemon subprocess** (`python -m
   secantus --host 127.0.0.1 --port <free> --storage-path :memory:`).
2. Waits for the daemon's TCP listener.
3. Invokes the driver's bundled `./gradlew --no-daemon
   -Dorg.mongodb.test.uri=mongodb://<host>:<port> :bson:test`. The
   `org.mongodb.test.uri` system property is the seam the driver's
   `ClusterFixture` test infrastructure reads at JVM startup.
4. After Gradle exits, copies JUnit XML out of
   `<module>/build/test-results/test/TEST-*.xml` into
   `.validation/java-results/` (keeps the submodule clean).
5. Tears down the daemon.
6. `java_validation.generate_report` walks the JUnit XML and emits
   `docs/validation-report-java.md` grouped by Gradle module.

## Files

- `runner.py` — daemon bootstrap + Gradle invocation + JUnit copy.
- `include_modules.py` — list of in-scope Gradle test targets.
- `generate_report.py` — JUnit XML → markdown table renderer.

## Not for distribution

Nothing under `java_validation/` or `vendor/mongo-java-driver/` ships
in the SecantusDB wheel or sdist. Dev-only.
