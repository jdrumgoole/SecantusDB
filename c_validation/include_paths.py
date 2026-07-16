"""Curated scope for the libmongoc (mongo-c-driver) conformance gauge.

``test-libmongoc`` selects tests by name prefix with the repeatable ``-l``
flag (e.g. ``-l '/Collection/*'``) and excludes named tests via a
``--skip-tests`` file. ``INCLUDE`` is the list of ``-l`` patterns the
gauge runs; ``SKIP_TESTS`` is the list of fully-qualified test names
written to the skip-file, each with a one-line rationale.

Curate the lists by building the binary once and running
``test-libmongoc --list-tests`` to enumerate the ~thousands of registered
tests, then keep the prefixes that exercise the wire-protocol surface
SecantusDB actually implements (CRUD, cursors, aggregation, commands,
query/write builders). Most libmongoc tests are *mock-server* tests that
spin up their own in-process server and never touch ``MONGOC_TEST_URI`` —
those pass regardless; the value is in the **live** tests
(``TestSuite_AddLive`` / ``AddFull``) that connect to the daemon under test.

Out-of-scope areas excluded by simply not listing their prefixes: SDAM /
server-selection spec tests (assume multi-node topology), transactions /
sessions (replica-set only), GridFS chunking edge cases, change-stream
resumability against real oplog rollover, CSE/CSFLE (client-side
encryption), SRV/DNS, OCSP/TLS, SASL/GSSAPI/X509 auth, load-balanced and
sharded-cluster topologies, retryable-writes failpoint injection.
"""

from __future__ import annotations

# ``-l`` name-prefix patterns, curated from ``test-libmongoc --list-tests``
# (mongo-c-driver 1.30.8 registers ~2800 tests). Scoped to the wire-protocol
# command surface SecantusDB implements: CRUD, cursors, aggregation, commands,
# change streams, GridFS, index/collection management. Aggregation lives under
# ``/Collection/aggregate`` and the ``/crud`` spec runner, so there's no
# separate ``/Aggregate`` prefix.
#
# Deliberately *not* listed (so they're excluded from the count, the same way
# the pymongo/Java/PHP gauges drop their non-server BSON-codec units): the
# in-process unit suites (``/bson``, ``/bson_corpus``, ``/rpc_message``,
# ``/Matcher``, ``/Util``, ``/mcommon``, ``/Log``, ``/Socket``, ``/Stream``,
# ``/Error``, ``/inheritance``) — they never open a connection — and the
# out-of-scope multi-node / feature suites (``/server_discovery_and_monitoring``,
# ``/server_selection``, ``/Topology``, ``/Cluster``, ``/max_staleness``,
# ``/Session``, ``/transactions``, ``/with_transaction``, ``/retryable_*``,
# ``/Stepdown``, ``/client_side_encryption``, ``/aws``, ``/scram``,
# ``/speculative_auth``, ``/ssl_opt``, ``/TLS``, ``/loadbalanced``,
# ``/load_balancers``, ``/*versioned_api``, URI/DNS parsing suites).
INCLUDE: list[str] = [
    "/Collection/*",
    "/Database/*",
    "/Client/*",
    "/BulkOperation/*",
    "/bulkwrite/*",
    "/Cursor/*",
    "/crud/*",
    "/find_and_modify/*",
    "/command_monitoring/*",
    "/WriteConcern/*",
    "/ReadConcern/*",
    "/ReadPrefs/*",
    "/WriteCommand/*",
    "/gridfs/*",
    "/gridfs_old/*",
    "/index-management/*",
    "/collection-management/*",
    "/long_namespace/*",
]

# NOTE on change streams: SecantusDB *does* serve change streams over the
# wire, but the C driver's test fixture bootstraps every ``/change_stream``
# test through ``test_framework_replset_member_count()``. SecantusDB answers
# ``replSetGetStatus`` with the standalone-mongod ``NoReplicationEnabled``
# error (see ``commands._repl_set_get_status``), so that helper reports 0
# members and the change-stream tests **skip gracefully** rather than abort the
# run (the pre-``replSetGetStatus`` behaviour was a hard process abort). Since
# they'd only skip, the suites are left out of INCLUDE entirely. Exercising
# them through the C driver would need ``replSetGetStatus`` to report >=1 live
# member (a fuller fake-replset reply). Tracked in tasks/backlog.md §3.2.

# Fully-qualified test names to skip, written one-per-line to the
# ``--skip-tests`` file. Each MUST carry a rationale comment: the test
# legitimately diverges because it assumes mongod behaviour SecantusDB
# does not emulate (multi-node topology, failpoints, real auth, etc.).
# Populate as the first real run surfaces actionable-looking failures
# that are in fact out-of-scope.
SKIP_TESTS: list[str] = []
