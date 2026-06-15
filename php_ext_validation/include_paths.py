"""In-scope ``.phpt`` directories under vendor/mongo-php-driver/tests/.

The PHP extension's ``.phpt`` files self-guard by topology via the
``skip_if_*`` helpers in ``tests/utils/skipif.php`` (``skip_if_not_standalone``,
``skip_if_not_replica_set``, ``skip_if_no_transactions``, …), so directories
that need a real multi-node deployment SKIP cleanly rather than fail. But some
suites still wait on orchestration the gauge can't provide (mongo-orchestration,
CSFLE, load-balancer fixtures) and would either hang or drown the signal in
skips, so the gauge passes ``run-tests.php`` an explicit directory list.

Each entry is a directory of tests that either is pure-driver (BSON
serialization — no server) or opens a real connection to the SecantusDB daemon
and exercises the wire protocol. This is the lowest layer of the PHP stack —
the C extension that wraps libmongoc — so it's the strictest wire-protocol
gauge (the class of bug pymongo's permissive client can't catch).

Widen one directory at a time, confirming it terminates within the runner's
wall-clock guard.
"""

from __future__ import annotations

# Paths relative to vendor/mongo-php-driver/. Passed positionally to
# run-tests.php, which recurses each directory for ``*.phpt``.
INCLUDE: list[str] = [
    # Pure-driver BSON serialization — no server needed. Proves the extension
    # encodes/decodes every BSON type correctly in our environment (the same
    # role :bson:test plays in the Java gauge).
    "tests/bson",
    # Wire-protocol surface — these open a real connection to SecantusDB and
    # exercise the command / query / cursor / write paths end-to-end.
    "tests/bulk",
    "tests/command",
    "tests/cursor",
    "tests/query",
    "tests/writeConcern",
    "tests/writeConcernError",
    "tests/writeError",
    "tests/writeResult",
    "tests/readConcern",
    "tests/readPreference",
    "tests/exception",
    "tests/functional",
    # Deferred (need orchestration / topology / features SecantusDB doesn't
    # provide — widen later as behaviour lands):
    # - tests/bson-corpus (983 pure tests; would dominate the denominator)
    # - tests/manager (174; connection-pool / SDAM heavy)
    # - tests/apm (command monitoring; mostly works, widen separately)
    # - tests/session, tests/retryable-reads, tests/retryable-writes,
    #   tests/causal-consistency (sessions / transactions on a real RS)
    # - tests/replicaset, tests/server, tests/standalone, tests/connect
    #   (SDAM / topology / auth / SSL assertions)
    # - tests/clientEncryption, tests/bson-binary-vector (CSFLE / vectors)
]
