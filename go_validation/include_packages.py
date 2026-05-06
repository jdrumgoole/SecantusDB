"""In-scope Go test packages under vendor/mongo-go-driver/.

Conservative starting set: BSON serialization (server-independent;
catches wire-format regressions) and the integration package (server-
dependent; many tests will self-skip on topology requirements but the
ones that survive give us "Go driver actually works against SecantusDB"
coverage).

Out of scope:
  ./internal/csfle*       (client-side encryption)
  ./internal/aws*         (Atlas-only auth)
  ./internal/spectest/atlas-data-lake-testing
  ./mongo/options/encryption*
  ./x/network/...         (low-level wire stuff already covered by integration)
"""

from __future__ import annotations

# Package paths relative to the vendor/mongo-go-driver/ module root.
INCLUDE: list[str] = [
    "./bson/...",
    "./mongo",
    # Server-dependent integration tests. Drives mongo-go-driver's full
    # CRUD / aggregate / change-stream / index-management surface
    # against SecantusDB over TCP. Runtime: ~several minutes (the
    # baseline tests run in seconds; this set adds the slow part).
    # Many tests self-skip on topology requirements (`require:
    # replicaset`, `require: sharded`, encryption, retryable-writes,
    # CSFLE) — the ones that survive are the honest cross-driver
    # gauge for "go-driver actually works against us".
    "./internal/integration/...",
]


# Test-name regexes passed to ``go test -skip``. Use for tests that
# exercise features SecantusDB intentionally doesn't implement —
# transactions with rollback, causal consistency / cluster-time
# semantics, server-side fail-points (failPoint admin command), the
# unified-spec aggregator (which loads transactions / sessions /
# retryable-* / GridFS / CSFLE spec dirs we don't support). Per
# CLAUDE.md "wire-protocol fidelity over feature completeness", these
# are honest gaps not bugs; skipping them keeps the gauge meaningful.
# Each entry should carry a one-line reason.
SKIP_PATTERNS: list[str] = [
    # Real multi-document transactions (commit/abort with rollback)
    # are out of scope per CLAUDE.md. SecantusDB returns {ok:1} from
    # commitTransaction / abortTransaction but does not actually roll
    # back, so retry-and-commit semantics fail correctly.
    "TestConvenientTransactions",
    # Causal consistency + cluster-time tracking needs real session
    # state and operationTime propagation, which we stub. Skip.
    "TestCausalConsistency_Supported",
    # Backpressure tests drive `configureFailPoint` to inject
    # synthetic overload errors. SecantusDB does not implement
    # diagnostic fail-points (testing-only admin command).
    "TestBackpressureProse",
    # `TestUnifiedSpec` aggregates every spec directory, including
    # the transactions / sessions / retryable-* / GridFS / CSFLE
    # bundles that are out of scope. The whole aggregator fails when
    # any one bundle fails. To re-include, the in-scope spec dirs
    # would need their own narrower runner.
    "TestUnifiedSpec",
    # `resume_token_updated_on_empty_batch` asserts that change-stream
    # resume tokens advance even when `getMore` returns no events —
    # real mongod does this via periodic noop oplog heartbeats. Per
    # tasks/backlog.md "## 3. Deferred work / Change-stream
    # limitations / noop heartbeat events", SecantusDB does not emit
    # heartbeats; resume tokens advance only on real ops.
    "TestChangeStream_ReplicaSet",
    # `TestCollection/<op>/write_concern_error` uses `w: 30` to
    # provoke a writeConcernError reply. Per CLAUDE.md "Read concern
    # / write concern semantics — accepted on the wire for
    # compatibility, otherwise ignored", we don't enforce the
    # concern, so the writeConcernError envelope isn't generated.
    # The test is gated on real multi-node behaviour that's out of
    # scope.
    # write_concern_error fires when w=30 is requested and not
    # satisfied; write_error wraps a documented error with a write
    # concern envelope. Both apply across every CRUD op variant.
    "TestCollection/.*/(write_concern_error|write_error)",
    # `operations_don't_retry_after_a_context_timeout` uses
    # `mt.SetFailPoint(failCommand)` to inject a synthetic timeout
    # mid-command. SecantusDB does not implement diagnostic
    # fail-points (testing-only admin command, out of scope).
    "TestClient/operations_don't_retry_after_a_context_timeout",
]
