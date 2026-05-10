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
    "com.mongodb.client.MongoCollectionTest",
    "com.mongodb.client.MongoClientTest",
    "com.mongodb.client.ExplainTest",
    "com.mongodb.client.ReadConcernTest",
    "com.mongodb.client.MongoWriteConcernWithResponseExceptionTest",
]


INCLUDE: list[ModuleSpec] = [
    ModuleSpec(
        task=":driver-sync:test",
        test_classes=list(_DRIVER_SYNC_FUNCTIONAL_INCLUDES),
    ),
]
