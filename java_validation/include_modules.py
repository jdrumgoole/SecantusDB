"""In-scope Gradle modules under vendor/mongo-java-driver/.

Conservative starting set: bson (pure unit, server-independent —
catches BSON serialization regressions, fast). The driver-core,
driver-sync, and driver-reactive-streams modules need a real-mongod
expectation for the bulk of their tests; many integration tests
require replica-set primary advertisements, change-stream cursors,
or multi-document transactions with rollback that are out of scope.

Out of scope:
  driver-kotlin*, driver-scala       — language-specific subsets
  driver-legacy                      — pre-MongoClient API (deprecated)
  driver-reactive-streams            — async; integration-only useful
  driver-lambda, driver-benchmarks   — runtime / perf, not conformance
  bson-kotlin*, bson-scala           — language wrappers around bson
  bson-record-codec                  — Java records, integration-y
"""

from __future__ import annotations

# Gradle test targets — passed to `./gradlew :module:test`.
INCLUDE: list[str] = [
    ":bson:test",
    # ":driver-core:test" was tried and hangs. The Gradle task merges
    # `src/test/unit/` (295 files, mostly safe) with `src/test/functional/`
    # (110 files, real-mongod-required) into one source set, and there's no
    # finer-grained Gradle task to select just unit/. The functional run
    # got ~25 minutes deep — change-stream codec tests passing, change-
    # stream prose tests producing real failures we'd want to triage —
    # then the test JVM hung indefinitely on what looks like an `awaitData`
    # change-stream cursor that SecantusDB doesn't terminate the way the
    # spec expects. Gradle never finalised the test task, so no JUnit XML
    # was written and the runner produced no report.
    #
    # Path forward when ready to re-enable:
    #   1. Force per-test timeouts via `--tests` Spock filter excluding
    #      ChangeStreamOperationProseTestSpecification and similar
    #      awaitData-using specs, OR
    #   2. Add a hard wall-clock timeout in java_validation/runner.py so
    #      a hung Gradle task gets terminated with a partial XML harvest
    #      from per-class results that have already been written, OR
    #   3. Patch the runner to use --tests "<pattern>" to whitelist
    #      driver-core packages that don't open long-lived cursors.
    # Until one of those lands, ship the bson-only baseline.
]
