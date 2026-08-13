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
    # The five entries below were triaged 2026-06-30 (Python server, HEAD
    # d8e75ff) and proven NOT to be server divergences: driving the same
    # operations via pymongo against an on-disk daemon produced exactly the
    # spec-expected wire replies, and the pymongo gauge passes the *identical*
    # upstream command-monitoring / versioned-api spec files (which assert the
    # same event counts — so the server induces no extra round-trips). Each is
    # a Java-driver-internal concern with no server wire surface. Details in
    # tasks/backlog.md §5.
    ExpectedFailure(
        pattern="metadata append does not create new connections",
        rationale=(
            "ClientMetadataTest: asserts the driver does NOT open a new "
            "connection or re-send `hello` after a client-side `appendMetadata` "
            "call. `appendMetadata` crosses no wire — purely Java-driver "
            "connection/handshake logic. Not server-fixable."
        ),
    ),
    ExpectedFailure(
        pattern="find and getMore append API version",
        rationale=(
            "VersionedApiTest: asserts the Java driver decorates outbound "
            'find/getMore with `apiVersion:"1"`. SecantusDB already accepts '
            "the serverApi fields (ok:1, correct cursor lifecycle); the "
            "assertion is on the driver's outbound command, not a server reply. "
            "pymongo passes the identical crud-api-version-1 spec."
        ),
    ),
    ExpectedFailure(
        pattern="A successful deleteMany",
        rationale=(
            "CommandMonitoringTest: server reply is spec-correct "
            "(single `delete` round-trip → {ok:1, n:2}); pymongo passes the "
            "identical command-monitoring spec. Failure is Java-driver event "
            "accounting against the standalone topology, not a server reply."
        ),
    ),
    ExpectedFailure(
        pattern="A successful find with a getMore",
        rationale=(
            "CommandMonitoringTest: server emits exactly the spec-expected "
            "find->getMore wire sequence (firstBatch 3 + Int64 cursor id, then "
            "nextBatch 2 + id:0, no extra round-trip); pymongo passes the "
            "identical spec. Failure is Java-driver event accounting."
        ),
    ),
    ExpectedFailure(
        pattern="Create a client, run a command, and close the client",
        rationale=(
            "ConnectionPoolLoggingTest: asserts a fixed sequence of the Java "
            "driver's CMAP connection-pool debug *log messages*. The server "
            "emits no log lines over the wire and cannot influence them. "
            "Out of scope (the sibling 'checkout fails' subtest is already "
            "driver-skipped)."
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

PYMONGO: list[ExpectedFailure] = [
    ExpectedFailure(
        pattern="test_index_hashed",
        rationale=(
            "Hashed indexes are intentionally out of scope per CLAUDE.md. "
            "`createIndexes` rejects them explicitly with "
            "`CannotCreateIndex` (67) 'hashed indexes are not supported by "
            "SecantusDB' — a faithful not-supported error, which is the "
            "documented preference over a half-implemented index type."
        ),
    ),
    ExpectedFailure(
        pattern="test_index_text",
        rationale=(
            "Text indexes are intentionally out of scope per CLAUDE.md "
            "(same gap as the node gauge's text-search test). "
            "`createIndexes` rejects them with `CannotCreateIndex` (67) "
            "'text indexes are not supported by SecantusDB'."
        ),
    ),
    ExpectedFailure(
        pattern="test_where",
        rationale=(
            "`$where` runs server-side JavaScript and SecantusDB ships no JS "
            "runtime, so it is rejected with `BadValue` (2) 'unsupported "
            "top-level operator: $where'. Out of scope per tasks/backlog.md §4 "
            "— supporting it would mean embedding a JS engine as mongod does."
        ),
    ),
    ExpectedFailure(
        pattern="test_maxtime_ms_message",
        rationale=(
            "Blocked by `$where`, not by the behaviour under test. The test "
            "builds a deliberately slow query with `find({'$where': delay(...)})` "
            "and asserts the resulting timeout error names the configured "
            "timeouts. SecantusDB rejects `$where` up front (BadValue 2), so the "
            "command fails before any timeout can elapse. NOTE: this leaves the "
            "maxTimeMS *error-message* shape unverified by this gauge rather "
            "than known-good — it is untested here, not proven correct."
        ),
    ),
    ExpectedFailure(
        pattern="test_to_list_csot_applied",
        rationale=(
            "Blocked by `$where`, same as `test_maxtime_ms_message`: the test "
            "delays the query with `find({'$where': delay(1)})` and asserts the "
            "raised error carries `.timeout == True`. `$where` is rejected up "
            "front, so the error is a BadValue rather than a timeout. NOTE: CSOT "
            "behaviour is therefore unverified by this gauge, not confirmed."
        ),
    ),
    ExpectedFailure(
        pattern="test_read_preference_hedge_deprecated",
        rationale=(
            "Async-only, and never reaches the wire: the test constructs "
            "`PrimaryPreferred(hedge={'enabled': True})` and asserts a "
            "`DeprecationWarning` is raised by the driver's own constructor. "
            "Purely client-side pymongo behaviour, dependent on the ambient "
            "warning filters — no server can influence the outcome."
        ),
    ),
]

C: list[ExpectedFailure] = [
    ExpectedFailure(
        pattern="/Client/select_server",
        rationale=(
            "libmongoc asserts the selected server is standalone / mongos / "
            "RS-secondary, but SecantusDB advertises itself as an RS *primary* "
            "in `hello` (deliberate — pymongo's change-stream topology machinery "
            "needs a replica-set primary). RSPrimary fails the test's "
            "`is_standalone_or_(rs_secondary_or_)mongos` check. A consequence of "
            "the single-node-replica-set advertisement, not a CRUD/wire gap."
        ),
    ),
    ExpectedFailure(
        pattern="/Client/last_write_date_absent",
        rationale=(
            "Asserts `lastWriteDate` is absent (a standalone trait); SecantusDB "
            "advertises as an RS primary and so returns `lastWrite.lastWriteDate` "
            "in `hello`. Same root cause as the `/Client/select_server` entries — "
            "the single-node-replica-set advertisement."
        ),
    ),
    ExpectedFailure(
        pattern="/Client/ipv6",
        rationale=(
            "Requires an IPv6 listener (`MONGOC_TEST_IPV6`); the gauge daemon "
            "binds IPv4 `127.0.0.1` only. Environment-specific, not a protocol gap."
        ),
    ),
]


def find_match(failures: list[ExpectedFailure], description: str) -> ExpectedFailure | None:
    """Return the first expected-failure entry whose pattern is a substring
    of ``description``. Returns ``None`` if no entry matches.
    """
    for ef in failures:
        if ef.pattern in description:
            return ef
    return None
