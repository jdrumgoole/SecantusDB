"""In-scope Gradle modules under vendor/mongo-java-driver/ for the Kotlin gauge.

The official Kotlin driver ships inside the mongo-java-driver monorepo as
``driver-kotlin-sync`` (and ``driver-kotlin-coroutine``). Only the
**integration** source set exercises the wire protocol:

  * ``driver-kotlin-sync/src/test/``         — pure Mockito-mocked unit tests
    of the Kotlin wrappers; never open a socket. Out of scope (they'd
    inflate the pass count without proving anything about SecantusDB).
  * ``driver-kotlin-sync/src/integrationTest/`` — open a real
    ``MongoClient`` (via the ``syncadapter`` shim over the coroutine/sync
    Kotlin client) and exchange wire commands with the SecantusDB daemon.
    This is the conformance signal, run via the ``:driver-kotlin-sync:
    integrationTest`` Gradle task.

Same ``ModuleSpec`` shape and staging discipline as
``java_validation.include_modules``: a class is added to the include set
only once the runner's wall-clock guard confirms it terminates against
SecantusDB. JUnit XML lands under
``<module>/build/test-results/integrationTest/`` (note: ``integrationTest``,
not ``test``) — the runner derives the harvest subdir from the task name.

Out of scope:
  driver-kotlin-coroutine            — the coroutine client drives the same
    wire path through a Flow/suspend API; add later if the sync gauge proves
    stable and we want the coroutine event-loop pacing covered too.
  bson-kotlin, bson-kotlinx          — pure serialization, server-independent.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

VENDOR = Path(__file__).resolve().parent.parent / "vendor" / "mongo-java-driver"


@dataclass(frozen=True)
class ModuleSpec:
    """A Gradle test target plus optional ``--tests`` filter.

    ``task`` is the path passed to ``./gradlew`` (e.g.
    ``:driver-kotlin-sync:integrationTest``). The runner derives the JUnit
    XML harvest subdirectory from the task's trailing segment
    (``integrationTest``), so a ``:foo:test`` spec harvests ``test/`` and a
    ``:foo:integrationTest`` spec harvests ``integrationTest/``.

    ``test_classes`` is a list of fully-qualified test class names. When
    non-empty, the runner adds one ``--tests <FQN>`` per entry to that
    module's gradle invocation. When empty, the module's full set runs.
    """

    task: str
    test_classes: list[str] = field(default_factory=list)


# Curated whitelist of driver-kotlin-sync integration test classes verified
# to terminate against SecantusDB. Same staging discipline as the Java and
# Ruby gauges — a class that hangs on a tailable getMore / unsupported
# feature can pin the gradle worker, so each is added only after the
# runner's wall-clock guard confirms it completes.
_DRIVER_KOTLIN_SYNC_INTEGRATION_INCLUDES: list[str] = [
    # Initial verified-good set: basic connectivity + CRUD smoke, and the
    # unified CRUD spec runner driven through the Kotlin sync adapter.
    "com.mongodb.kotlin.client.SmokeTests",
    "com.mongodb.kotlin.client.UnifiedCrudTest",
    # Excluded for now:
    # - UnifiedTest — abstract base class for the unified-spec runners
    #   (UnifiedCrudTest extends it); not a runnable target on its own.
]


INCLUDE: list[ModuleSpec] = [
    ModuleSpec(
        task=":driver-kotlin-sync:integrationTest",
        test_classes=list(_DRIVER_KOTLIN_SYNC_INTEGRATION_INCLUDES),
    ),
]
