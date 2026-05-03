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
]
