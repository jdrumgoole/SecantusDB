"""In-scope Gradle modules under vendor/mongo-java-driver/.

Each module is a `ModuleSpec(task, test_classes)` tuple. When `test_classes`
is non-empty, the runner adds one `--tests <FQN>` per entry to the gradle
invocation for that module — limiting Gradle's test set to the listed
classes. Run mode in the runner is one gradle invocation per module so a
filter on driver-core doesn't accidentally clamp bson too.

Modules:

* `:bson:test` — full module, no filter. Pure BSON serialization tests
  with no server dependency.

* `:driver-core:test` — filtered to the unit-test source set
  (`driver-core/src/test/unit/`, ~290 classes). Gradle merges unit/ and
  functional/ into one source set with no built-in selection, but the
  classes have disjoint names (verified at startup) so a `--tests`
  whitelist of unit FQNs cleanly excludes the functional ones. Functional
  tests are skipped because the driver's `ClusterFixture` infrastructure
  expects a real multi-node deployment — `getSecondary()` is an unbounded
  sleep loop waiting for a SECONDARY that SecantusDB cannot advertise (we
  are a fictional single-node primary by design). Unit tests are pure
  Java/Groovy and run cleanly against any `MONGODB_URI`.

Out of scope:
  driver-sync, driver-reactive-streams — bulk integration tests against
    a real mongod; subset overlaps with driver-core unit tests.
  driver-kotlin*, driver-scala       — language-specific subsets
  driver-legacy                      — pre-MongoClient API (deprecated)
  driver-lambda, driver-benchmarks   — runtime / perf, not conformance
  bson-kotlin*, bson-scala           — language wrappers around bson
  bson-record-codec                  — Java records, integration-y
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

VENDOR = Path(__file__).resolve().parent.parent / "vendor" / "mongo-java-driver"


@dataclass(frozen=True)
class ModuleSpec:
    """A Gradle test target plus optional `--tests` filter.

    `task` is the path passed to `./gradlew` (e.g. ":driver-core:test").

    `test_classes` is a list of fully-qualified test class names. When
    non-empty, the runner adds one `--tests <FQN>` per entry to that
    module's gradle invocation. When empty, the module's full test set
    runs unfiltered.
    """

    task: str
    test_classes: list[str] = field(default_factory=list)


# Test classes that live under unit/ but transitively require a real
# mongod's diagnostic / fault-injection protocol — primarily the
# `configureFailPoint` command, which the connection-pool spec tests
# drive to simulate slow handshakes / failed connections. SecantusDB
# does not (and arguably shouldn't) implement these testing-only
# admin commands, so the spec tests can't run against the surrogate.
# Adding a class here removes it from the gradle `--tests` whitelist.
_DRIVER_CORE_UNIT_EXCLUDES: frozenset[str] = frozenset({
    # ConnectionPool spec tests use {"configureFailPoint": ...} to make
    # the server stall connection setup, then assert that maxConnecting
    # throttles concurrent handshakes. Without the failpoint they pass
    # connections through immediately and the throttle invariants fail.
    "com.mongodb.internal.connection.ConnectionPoolTest",
    "com.mongodb.internal.connection.ConnectionPoolAsyncTest",
})


def _enumerate_unit_test_classes() -> list[str]:
    """Walk `driver-core/src/test/unit/` and return FQNs of every test
    source file, minus `_DRIVER_CORE_UNIT_EXCLUDES`. Abstract* bases and
    helper utilities are kept — Gradle's `--tests` silently no-ops on
    classes without runnable methods, so including them is harmless and
    avoids coupling enumeration to test class naming conventions.

    Returns an empty list if the submodule isn't initialised (the runner
    handles that case with a clearer error).
    """
    unit_dir = VENDOR / "driver-core" / "src" / "test" / "unit"
    if not unit_dir.is_dir():
        return []
    fqns: list[str] = []
    for path in sorted(unit_dir.rglob("*")):
        if path.suffix not in (".java", ".groovy"):
            continue
        rel = path.relative_to(unit_dir)
        fqn = ".".join(rel.with_suffix("").parts)
        if fqn in _DRIVER_CORE_UNIT_EXCLUDES:
            continue
        fqns.append(fqn)
    return fqns


INCLUDE: list[ModuleSpec] = [
    ModuleSpec(task=":bson:test"),
    ModuleSpec(
        task=":driver-core:test",
        test_classes=_enumerate_unit_test_classes(),
    ),
]
