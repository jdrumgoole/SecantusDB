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
    # Change streams. The C driver's fixture bootstraps every ``/change_stream``
    # test through ``test_framework_replset_member_count()``, which counts the
    # ``members`` array of ``replSetGetStatus``. These suites were excluded for
    # as long as that answer was the standalone ``NoReplicationEnabled`` error:
    # the helper saw 0 members and skipped them all as "standalone", so
    # including them bought nothing. ``replSetGetStatus`` now reports the
    # one-member roster its own ``hello`` already advertises, so they run.
    "/change_stream/*",
    "/change_streams/*",
]

# Fully-qualified test names to skip, written one-per-line to the
# ``--skip-tests`` file. Each MUST carry a rationale comment: the test
# legitimately diverges because it assumes mongod behaviour SecantusDB
# does not emulate (multi-node topology, failpoints, real auth, etc.).
# Populate as the first real run surfaces actionable-looking failures
# that are in fact out-of-scope.
SKIP_TESTS: list[str] = [
    # These three need a real SECONDARY to exist. SecantusDB advertises a
    # single-node replica set (one PRIMARY, no other members) so drivers accept
    # change streams; a secondary read therefore has nowhere to go. Multi-node
    # topology is explicitly out of scope (CLAUDE.md), so these can never pass
    # and are not evidence of a wire divergence.
    #
    # They only became visible when `replSetGetStatus` started reporting the
    # one-member roster its own `hello` already advertised — the same change
    # that made the /change_stream suites runnable at all.
    "/Client/command_secondary",
    "/Collection/aggregate/secondary",
    "/change_stream/live/read_prefs",
]
