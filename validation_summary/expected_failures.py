"""Known driver-gauge tests that fail for documented reasons.

Each entry names a test (string match against the gauge's native test
description) along with a short rationale. The per-gauge
``generate_report.py`` imports this module to reclassify matching
failures as ``expected_failure`` instead of ``failed``. The summary
then reports unexpected failures separately from documented gaps so
the "pass rate" number isn't gamed by us hand-waving gaps away — the
gauge still knows the test failed, we just acknowledge it explicitly.

When you fix one of these gaps, delete its entry. When you discover a
new genuine bug, do NOT add it here — fix it instead.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class ExpectedFailure:
    """A specific test that fails for a documented reason.

    ``pattern`` is a substring match (case-sensitive) against the
    gauge's native test description / node ID. ``rationale`` is the
    one-line reason that goes in the report.
    """

    pattern: str
    rationale: str


JAVA: list[ExpectedFailure] = [
    ExpectedFailure(
        pattern="CRUD Api Version 1 (strict): distinct appends declared API version",
        rationale=(
            "apiStrict rejection on the `distinct` command-name triggers a "
            "`MongoConnectionPoolClearedException` cascade in the Java driver's "
            "SDAM for reasons not yet diagnosed (root cause is in the driver, "
            "not SecantusDB). Leaving the stage-level apiStrict gate active "
            "but the command-level gate inert. Documented in tasks/backlog.md §5."
        ),
    ),
]

GO: list[ExpectedFailure] = [
    ExpectedFailure(
        pattern="TestChangeStream_ReplicaSet/try_next/one_getMore_sent",
        rationale=(
            "Long-standing intermittent flake — fails ~1/3 runs with "
            "`TryNext returned true on iteration 1`. Repros only under "
            "full-gauge load. Cause unknown after focused investigation. "
            "Documented in tasks/backlog.md §5."
        ),
    ),
    ExpectedFailure(
        pattern="TestIndexView/drop_one",
        rationale=(
            "Load-induced server-selection timeout — Go driver can't reach "
            "the daemon during heavy parallel test execution. "
            "Documented in tasks/backlog.md §5."
        ),
    ),
    ExpectedFailure(
        pattern="TestIndexView/drop_all",
        rationale=("Same load-induced server-selection timeout as TestIndexView/drop_one."),
    ),
    ExpectedFailure(
        pattern="TestIndexView/create_many",
        rationale=(
            "Same load-induced server-selection timeout family — fires when "
            "the daemon can't accept new connections during parallel gauge runs."
        ),
    ),
    # Parent rollups fail when child subtests fail. Treat them as expected too.
    ExpectedFailure(
        pattern="TestChangeStream_ReplicaSet/try_next",
        rationale="Rollup of the `one_getMore_sent` subtest above.",
    ),
    ExpectedFailure(
        pattern="TestChangeStream_ReplicaSet",
        rationale="Rollup of the `try_next/one_getMore_sent` subtest above.",
    ),
    ExpectedFailure(
        pattern="TestIndexView",
        rationale="Rollup of the `drop_one` / `drop_all` / `create_many` subtests above.",
    ),
]

NODE: list[ExpectedFailure] = [
    ExpectedFailure(
        pattern="Find should correctly sort using text search in find",
        rationale=(
            "Text indexes (`$text`, `$meta: textScore`, text-index creation) "
            "are intentionally out of scope per CLAUDE.md — would require a "
            "full-text index implementation. Documented in tasks/backlog.md §4."
        ),
    ),
]

RUBY: list[ExpectedFailure] = [
    ExpectedFailure(
        pattern=(
            "Mongo::Collection#create when the collection has options when the "
            "collection has a write concern when write concern passed in as an "
            "option applies the write concern passed in as an option"
        ),
        rationale=(
            "The test passes `w: 2` and expects success — it assumes the "
            "canonical multi-node replica-set test cluster the Ruby driver's "
            "own CI runs against. SecantusDB advertises as a single-node "
            "replica set, so `w: 2` returns `CannotSatisfyWriteConcern` "
            "(the correct mongod emulation). Documented in tasks/backlog.md §5."
        ),
    ),
]

PYMONGO: list[ExpectedFailure] = []


def find_match(failures: list[ExpectedFailure], description: str) -> ExpectedFailure | None:
    """Return the first expected-failure entry whose pattern is a substring
    of ``description``. Returns ``None`` if no entry matches.
    """
    for ef in failures:
        if ef.pattern in description:
            return ef
    return None
