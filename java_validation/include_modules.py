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
    # ":driver-core:test" — DEFERRED. The Gradle task merges `src/test/unit/`
    # with `src/test/functional/` into one source set with no fine-grained
    # selection. Two iterations of running it surfaced — and we fixed —
    # real wire-protocol bugs in SecantusDB:
    #   • change streams emitted operationType:"update" for replacement-style
    #     updates (mongod emits "replace"); a $match: {operationType:"replace"}
    #     pipeline never saw the event and the cursor blocked forever.
    #   • OP_MSG with the moreToCome flag (set for writeConcern:{w:0}
    #     unacknowledged writes) was being replied to, desyncing the
    #     connection's responseTo↔requestId chain on the next normal request.
    # Past those fixes, driver-core tests reach a class like
    # `SingleServerClusterTest.shouldSuccessfullyQueryASecondaryWithPrimaryReadPreference`
    # which calls `ClusterFixture.getSecondary()` — an unbounded sleep loop
    # waiting for a SECONDARY in the topology. SecantusDB advertises a
    # single-node "secantus" replica set with no other members by design,
    # so any test that pins a non-primary host wedges indefinitely.
    # Re-enabling this module needs either (1) per-class --tests filter that
    # whitelists only topology-agnostic suites, or (2) running with
    # `replica_set_name=None` so the cluster fixture picks the standalone
    # path. Neither has clean upstream support yet; ship bson-only.
]
