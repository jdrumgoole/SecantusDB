"""In-scope test paths under vendor/pymongo-tests/test/.

The list is intentionally conservative — anything that requires a
topology, behaviour, or feature SecantusDB doesn't aim to support
(real multi-node replica sets, sharding, transactions w/ rollback,
GridFS, encryption, auth, TLS, retryable writes / reads, server
selection algorithms, sessions w/ correlation, async, performance,
mockup-driven tests, search indexes, time series, clustered indexes)
is excluded. Change streams ARE in scope (single-node, oplog-backed
implementation; pymongo accepts them because ``hello`` advertises a
fictional single-node replica-set primary).

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
    # Change streams. test_change_stream.py also loads the JSON unified
    # specs from change_streams/unified/ via its own runner. Tests that
    # require multi-node oplog semantics (e.g. mongos cluster-wide
    # change streams) self-skip via pymongo's topology decorators.
    "vendor/pymongo-tests/test/test_change_stream.py",
    # Explicitly EXCLUDED (out of scope per CLAUDE.md):
    #   test_index_management.py     — Atlas search indexes, integration only
    #   test_transactions*           — real transaction rollback
    #   test_session*                — session correlation
    #   test_retryable_*             — retryable writes/reads (replica set)
    #   test_replica_set*, test_topology, test_discovery_and_monitoring
    #   test_server_selection*       — sharded / mongos
    #   test_max_staleness, test_load_balancer
    #   test_csot                    — client-side timeouts (replica-set semantics)
    #   test_grid_file*, test_gridfs* — GridFS
    #   test_encryption, test_on_demand_csfle — CSFLE
    #   test_auth*, test_ssl, test_ocsp* — auth / TLS
    #   asynchronous/, atlas/, lambda/, performance/, mockupdb/
    #   index_management/, collection_management/, read_write_concern/
    #     (spec dirs containing JSON for features above)
]
