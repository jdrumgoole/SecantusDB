"""In-scope test paths under vendor/pymongo-tests/test/.

The list is intentionally conservative — anything that requires a
topology, behaviour, or feature SecantusDB doesn't aim to support
(real multi-node replica sets, sharding, GridFS, encryption, auth,
TLS, retryable writes / reads, server selection algorithms, sessions
w/ correlation, async, performance, mockup-driven tests, search
indexes, time series, clustered indexes) is excluded. Change streams
ARE in scope (single-node, oplog-backed implementation; pymongo
accepts them because ``hello`` advertises a fictional single-node
replica-set primary), and so are multi-document transactions
(WT-native, single-node; see ``secantus.transactions``).

The result of running pytest against just this list is the
"MongoDB compatibility" gauge published in docs/validation-report.md.
Adding test files here when new SecantusDB features land is part of
closing the gap.

The bare spec-data directories under `vendor/pymongo-tests/test/` (e.g.
`crud/`, `bson_corpus/`, `run_command/`) are not pytest collectables on
their own — they're JSON documents loaded by a runner file like
`test_crud_unified.py`. Include the runner files, not the spec dirs.
"""

from __future__ import annotations

# Prefixed with the submodule path so they can be passed straight to pytest.
INCLUDE: list[str] = [
    # Core CRUD surface.
    "vendor/pymongo-tests/test/test_collection.py",
    "vendor/pymongo-tests/test/test_cursor.py",
    "vendor/pymongo-tests/test/test_bulk.py",
    "vendor/pymongo-tests/test/test_collection_management.py",
    "vendor/pymongo-tests/test/test_database.py",
    "vendor/pymongo-tests/test/test_examples.py",
    "vendor/pymongo-tests/test/test_results.py",
    "vendor/pymongo-tests/test/test_run_command.py",
    "vendor/pymongo-tests/test/test_operations.py",
    # Spec-test runners. These load the JSON specs in the corresponding
    # subdirectories (vendor/pymongo-tests/test/crud/unified/, etc.) and
    # execute them as conformance tests — exactly the conformance signal
    # we want.
    "vendor/pymongo-tests/test/test_crud_unified.py",
    "vendor/pymongo-tests/test/test_crud_v1.py",
    "vendor/pymongo-tests/test/test_command_monitoring.py",
    # BSON / type round-trip — server-independent but exercise wire serialization.
    "vendor/pymongo-tests/test/test_bson.py",
    "vendor/pymongo-tests/test/test_bson_corpus.py",
    "vendor/pymongo-tests/test/test_binary.py",
    "vendor/pymongo-tests/test/test_objectid.py",
    "vendor/pymongo-tests/test/test_decimal128.py",
    "vendor/pymongo-tests/test/test_timestamp.py",
    "vendor/pymongo-tests/test/test_code.py",
    "vendor/pymongo-tests/test/test_dbref.py",
    "vendor/pymongo-tests/test/test_son.py",
    "vendor/pymongo-tests/test/test_raw_bson.py",
    "vendor/pymongo-tests/test/test_json_util.py",
    "vendor/pymongo-tests/test/test_custom_types.py",
    "vendor/pymongo-tests/test/test_default_exports.py",
    # Comment/operator handling — small surface, cheap signal.
    "vendor/pymongo-tests/test/test_comment.py",
    # Common helpers + errors.
    "vendor/pymongo-tests/test/test_common.py",
    "vendor/pymongo-tests/test/test_errors.py",
    # Read/write concern + read preference. Mostly driver-side
    # semantics (concern object construction, URI parsing, default
    # propagation). Single-node SecantusDB accepts these on the wire
    # and ignores them — what runs here is the surface that doesn't
    # depend on multi-node consistency.
    "vendor/pymongo-tests/test/test_read_concern.py",
    "vendor/pymongo-tests/test/test_write_concern.py",
    "vendor/pymongo-tests/test/test_read_preferences.py",
    # Change streams. test_change_stream.py also loads the JSON unified
    # specs from change_streams/unified/ via its own runner. Tests that
    # require multi-node oplog semantics (e.g. mongos cluster-wide
    # change streams) self-skip via pymongo's topology decorators.
    "vendor/pymongo-tests/test/test_change_stream.py",
    # Collation. Wire-side ``collation`` echo + per-index collation
    # (single-field, compound, sort acceleration) all ship as of
    # 0.5.2b1 — see `docs/indexes.md` "Per-index collation". Pymongo's
    # built-in collation suite runs cleanly.
    "vendor/pymongo-tests/test/test_collation.py",
    # Stable API v1. ``apiVersion`` echo, ``apiStrict`` aggregation-
    # stage gate (b25-era), and the ``distinct`` command-name gate
    # (0.5.2b3) cover the surface these tests exercise.
    "vendor/pymongo-tests/test/test_versioned_api.py",
    "vendor/pymongo-tests/test/test_versioned_api_integration.py",
    # Command monitoring + logging. We emit the started / succeeded /
    # failed events the driver gates on; the logging-suite tests
    # exercise format-string conformance and don't depend on any
    # SecantusDB-specific behaviour past that.
    "vendor/pymongo-tests/test/test_command_logging.py",
    "vendor/pymongo-tests/test/test_logger.py",
    # Multi-document transactions. Real WT-native transactions (per-txn
    # sessions, oplog buffering, spec-pinned error codes + transient
    # labels) ship via secantus.transactions; this runner executes the
    # transactions/unified JSON specs. Tests requiring mongos topologies
    # self-skip via pymongo's topology decorators; remaining divergences
    # are listed in tasks/backlog.md §3.4.
    "vendor/pymongo-tests/test/test_transactions_unified.py",
    # Explicitly EXCLUDED (out of scope per CLAUDE.md):
    #   test_index_management.py     — Atlas search indexes, integration only
    #   test_transactions.py         — pymongo's legacy spec runner; leans on
    #     replica-set fixtures (client_context.replica_set_name) beyond the
    #     unified runner's topology decorators. The unified suite above is
    #     the conformance proof.
    #   test_session*                — session correlation
    #   test_retryable_*             — retryable writes/reads (replica set)
    #   test_replica_set*, test_topology, test_discovery_and_monitoring
    #   test_server_selection*       — sharded / mongos
    #   test_max_staleness, test_load_balancer
    #   test_csot                    — client-side timeouts (replica-set semantics)
    #   test_grid_file*, test_gridfs* — GridFS
    #   test_encryption, test_on_demand_csfle — CSFLE
    #   test_auth.py                 — SCRAM round-trip works (RBAC ships,
    #     plugin runs the suite with auth=on), and speculativeAuthenticate
    #     is now in. Remaining gaps before turning this on: SCRAM-SHA-1
    #     mechanism, $where (JS predicate). Each is its own slice.
    #   test_auth_oidc, test_auth_aws — OIDC / AWS auth providers
    #   test_auth_spec.py            — non-SCRAM cred mechanism specs
    #   test_ssl, test_ocsp*         — TLS
    #   asynchronous/, atlas/, lambda/, performance/, mockupdb/
    #   index_management/, collection_management/, read_write_concern/
    #     (spec dirs containing JSON for features above)
]


# Specific test IDs to deselect from the gauge — passed to pytest's
# ``--deselect`` flag by the ``validate`` task. Use for tests that
# fail in our environment for reasons unrelated to SecantusDB
# compatibility (pymongo-side test bugs, environment-specific warning
# filters, etc.). Each entry carries a one-line reason so the next
# pymongo upgrade can reassess.
DESELECT_TESTS: list[str] = [
    # pymongo's `assertRaises(DeprecationWarning)` here depends on a
    # warning filter that's only set by pymongo's own conftest stack;
    # under our plugin invocation the warning fires but isn't
    # converted to an exception, so the assertion fails. The test
    # verifies a pymongo-internal deprecation, not server behaviour.
    "vendor/pymongo-tests/test/test_read_preferences.py::TestMongosAndReadPreference::test_read_preference_hedge_deprecated",
    # The DBRef spec tests are pure client-side BSON encode/decode — they
    # exercise pymongo's `bson.DBRef` codec, never SecantusDB's wire
    # protocol or storage, so they don't measure server compatibility.
    # They pass cleanly under plain unittest, but the gauge runs `-n1`
    # (xdist) and these iterate `with self.subTest(doc=doc)` over docs
    # containing `ObjectId`; pytest-xdist's execnet serialization can't
    # pickle an `ObjectId` in the subtest params when reporting outcomes
    # across the worker boundary, so the run dies with
    # `execnet DumpError: can't serialize <class 'bson.objectid.ObjectId'>`.
    # That's an xdist/execnet limitation, not a SecantusDB failure —
    # counting them red understates real compatibility.
    "vendor/pymongo-tests/test/test_dbref.py::TestDBRefSpec::test_decoding_1_2_3",
    "vendor/pymongo-tests/test/test_dbref.py::TestDBRefSpec::test_decoding_4_5",
    "vendor/pymongo-tests/test/test_dbref.py::TestDBRefSpec::test_encoding_1_2",
]
