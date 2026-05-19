"""In-scope Gradle modules under vendor/mongo-java-driver/.

Each module is a `ModuleSpec(task, test_classes)` tuple. When `test_classes`
is non-empty, the runner adds one `--tests <FQN>` per entry to the gradle
invocation for that module — limiting Gradle's test set to the listed
classes. Run mode in the runner is one gradle invocation per module so a
filter on driver-sync doesn't accidentally clamp bson too.

The gauge runs the **driver-sync functional** subset — the integration
tests under ``driver-sync/src/test/functional/`` that open real
``MongoClient`` instances and exchange wire commands with the
SecantusDB daemon. Pure-BSON unit tests under ``:bson:test`` and
``:driver-core:test`` (``unit/`` source set) verify the driver's own
serialization logic without ever touching the wire, so they're out of
scope here — they'd inflate the pass count without proving anything
about SecantusDB conformance.

The driver's ``ClusterFixture`` infrastructure was originally a
blocker (``getSecondary()`` is an unbounded sleep loop on
non-multi-node deployments), but the runner sidesteps it by spawning
the SecantusDB daemon in **standalone mode** (``--standalone``), so
the driver's topology classification is ``STANDALONE`` and tests
gated on ``assumeTrue(isReplicaSet())`` skip cleanly.

Out of scope:
  driver-reactive-streams, driver-kotlin*, driver-scala — language
    or paradigm wrappers around the same wire path
  driver-legacy                      — pre-MongoClient API
  driver-lambda, driver-benchmarks   — runtime / perf, not conformance
  bson-kotlin*, bson-scala, bson-record-codec — language wrappers
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

VENDOR = Path(__file__).resolve().parent.parent / "vendor" / "mongo-java-driver"


@dataclass(frozen=True)
class ModuleSpec:
    """A Gradle test target plus optional `--tests` filter.

    `task` is the path passed to `./gradlew` (e.g. ``:driver-sync:test``).

    `test_classes` is a list of fully-qualified test class names. When
    non-empty, the runner adds one `--tests <FQN>` per entry to that
    module's gradle invocation. When empty, the module's full test set
    runs unfiltered.
    """

    task: str
    test_classes: list[str] = field(default_factory=list)


# Curated whitelist of driver-sync functional test classes verified
# to terminate against SecantusDB. Same staging discipline as
# ruby_validation/include_paths.py: a single test that hangs on a
# tailable getMore / unsupported feature can pin the gradle worker,
# so each class is added only after the runner's wall-clock guard
# confirms it completes. Add new candidates by FQN as they prove
# good citizens.
_DRIVER_SYNC_FUNCTIONAL_INCLUDES: list[str] = [
    # Initial verified-good set.
    "com.mongodb.client.MongoCollectionTest",
    "com.mongodb.client.MongoClientTest",
    "com.mongodb.client.ExplainTest",
    "com.mongodb.client.ReadConcernTest",
    "com.mongodb.client.MongoWriteConcernWithResponseExceptionTest",
    # Widen — basic CRUD / connectivity / index surface.
    "com.mongodb.client.ConnectivityTest",
    "com.mongodb.client.ContextProviderTest",
    "com.mongodb.client.InContextMqlValuesFunctionalTest",
    "com.mongodb.client.unified.IndexManagementTest",
    # Unified spec runners — driver-level event / logging / monitoring
    # protocols. Tests that need features SecantusDB intentionally
    # doesn't implement (transactions, retryable-writes write-errors,
    # log-id propagation hooks) self-skip via the YAML run-on
    # constraints.
    "com.mongodb.client.unified.CommandLoggingTest",
    "com.mongodb.client.unified.CommandMonitoringTest",
    "com.mongodb.client.unified.ConnectionPoolLoggingTest",
    # More candidates being widened (safer: no change-streams /
    # sessions / retryable in this batch — those have known
    # tailable-getMore hang risk).
    "com.mongodb.client.ClientMetadataTest",
    "com.mongodb.client.ClusterEventPublishingTest",
    "com.mongodb.client.unified.UnifiedCrudTest",
    # Wave 2 widening — each is a unified spec runner that drives
    # YAML test files. The runner's ``runOnRequirements`` blocks
    # self-skip scenarios that need features SecantusDB doesn't have
    # (transactions, multi-node replica, retryable etc.), so
    # individual misses surface as `skipped` rather than hangs.
    "com.mongodb.client.unified.ChangeStreamsTest",
    "com.mongodb.client.unified.UnifiedWriteConcernTest",
    "com.mongodb.client.unified.VersionedApiTest",
    # Wave 3 widening — features SecantusDB ships at the wire level:
    # SCRAM-SHA-256 auth (UnifiedAuthTest), GridFS-as-CRUD on the
    # ``.files``/``.chunks`` pair (UnifiedGridFSTest), and logical
    # session tracking (SessionsTest). Transaction-only and
    # multi-node-only scenarios self-skip via the YAML
    # ``runOnRequirements`` blocks the unified runner respects.
    "com.mongodb.client.unified.UnifiedAuthTest",
    "com.mongodb.client.unified.UnifiedGridFSTest",
    "com.mongodb.client.unified.SessionsTest",
    # Excluded (need features that are intentionally out of scope, or
    # that need a deeper investigation to be tractable):
    # - ServerSelectionLoggingTest — half its scenarios depend on the
    #   ``configureFailPoint`` ``closeConnection: true`` mode (we
    #   only support ``errorCode`` / ``writeConcernError`` modes
    #   today) plus an ``Unknown`` server-description event the
    #   driver fires on connection close.
    # - ExplicitUuidCodecUuidRepresentationTest — UUID legacy/standard
    #   binary subtype round-trip. SecantusDB stores BSON blobs
    #   unchanged (subtype preserved at the byte level), but the
    #   parametrized test surfaces a real id-key normalization issue
    #   that needs its own slice to fix safely.
    # - CollectionManagementTest — clustered indexes + time-series
    #   collections; both are MongoDB 5.0+ features this gauge
    #   doesn't aim to support yet.
    # - CrudProseTest — depends on writeErrors[].errInfo (rich
    #   validation-error details, MongoDB 5.0+); accept-on-the-wire
    #   only today.
]


# Curated whitelist of driver-core functional test classes. These live
# under driver-core's own test source set rather than driver-sync's but
# exercise wire-level behaviour against a live mongod / SecantusDB the
# same way. Two geo-filter specs are the natural fit — SecantusDB
# ships full geo support (operators + 2d/2dsphere indexes + $geoNear),
# so the upstream specs that drive $geoWithin / $geoIntersects /
# $near / $nearSphere through the driver's Filters builder against a
# real server should pass against us too.
_DRIVER_CORE_FUNCTIONAL_INCLUDES: list[str] = [
    # GeoJSON-style filters against a 2dsphere index.
    "com.mongodb.client.model.GeoJsonFiltersFunctionalSpecification",
    # Legacy [x, y] / $box / $polygon / $center filters against a 2d
    # index — same four operators, different doc-side shape.
    "com.mongodb.client.model.GeoFiltersFunctionalSpecification",
]


INCLUDE: list[ModuleSpec] = [
    ModuleSpec(
        task=":driver-sync:test",
        test_classes=list(_DRIVER_SYNC_FUNCTIONAL_INCLUDES),
    ),
    ModuleSpec(
        task=":driver-core:test",
        test_classes=list(_DRIVER_CORE_FUNCTIONAL_INCLUDES),
    ),
]
