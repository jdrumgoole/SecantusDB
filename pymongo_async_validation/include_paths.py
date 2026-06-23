"""In-scope test paths under vendor/pymongo-tests/test/asynchronous/.

The async gauge runs pymongo's native ``AsyncMongoClient`` suite — the
async/await wire path that replaced Motor — against the same embedded
SecantusDB the sync gauge uses. The scope mirrors
``pymongo_validation.include_paths`` (the sync gauge): the
server-touching CRUD / cursor / change-stream / command-monitoring /
collation / transaction surface, restricted to the files that actually
have an ``asynchronous/`` variant upstream.

What's intentionally *not* mirrored from the sync list: the pure-BSON /
type round-trip files (``test_bson.py``, ``test_objectid.py``,
``test_decimal128.py``, …). Those are server-independent codec tests that
live at ``test/`` root (there is no async variant), and the sync gauge
already covers them — counting them here would inflate the async number
with tests that never touch the async wire path.

Same exclusions as the sync gauge otherwise (replica-set / sharding /
auth / TLS / encryption / GridFS / retryable / session-correlation
suites self-skip or are out of scope). The result is published in
docs/validation-report-pymongo-async.md.

The async tests need ``pytest-asyncio`` with ``asyncio_mode=auto`` — the
``validate-pymongo-async`` task passes that on the pytest command line so
the unmodified vendored config isn't required.
"""

from __future__ import annotations

# Prefixed with the submodule path so they can be passed straight to pytest.
INCLUDE: list[str] = [
    # Core CRUD surface (async variants).
    "vendor/pymongo-tests/test/asynchronous/test_collection.py",
    "vendor/pymongo-tests/test/asynchronous/test_cursor.py",
    "vendor/pymongo-tests/test/asynchronous/test_bulk.py",
    "vendor/pymongo-tests/test/asynchronous/test_collection_management.py",
    "vendor/pymongo-tests/test/asynchronous/test_database.py",
    "vendor/pymongo-tests/test/asynchronous/test_examples.py",
    "vendor/pymongo-tests/test/asynchronous/test_run_command.py",
    # Spec-test runners (load the shared JSON specs, drive them async).
    "vendor/pymongo-tests/test/asynchronous/test_crud_unified.py",
    "vendor/pymongo-tests/test/asynchronous/test_command_monitoring.py",
    # Change streams — async tailable getMore / awaitData path.
    "vendor/pymongo-tests/test/asynchronous/test_change_stream.py",
    # Collation echo + per-index collation.
    "vendor/pymongo-tests/test/asynchronous/test_collation.py",
    # Stable API v1 integration.
    "vendor/pymongo-tests/test/asynchronous/test_versioned_api_integration.py",
    # Command monitoring + logging.
    "vendor/pymongo-tests/test/asynchronous/test_command_logging.py",
    "vendor/pymongo-tests/test/asynchronous/test_logger.py",
    # Multi-document transactions (unified spec runner, async).
    "vendor/pymongo-tests/test/asynchronous/test_transactions_unified.py",
    # Comment/operator handling; common helpers; concerns/prefs.
    "vendor/pymongo-tests/test/asynchronous/test_comment.py",
    "vendor/pymongo-tests/test/asynchronous/test_common.py",
    "vendor/pymongo-tests/test/asynchronous/test_read_concern.py",
    "vendor/pymongo-tests/test/asynchronous/test_read_preferences.py",
    "vendor/pymongo-tests/test/asynchronous/test_custom_types.py",
    # Explicitly EXCLUDED (out of scope, same rationale as the sync gauge):
    #   test_session*, test_retryable_*       — session correlation / replica set
    #   test_encryption.py, test_auth*        — CSFLE / auth providers
    #   test_grid*, test_gridfs*              — GridFS
    #   test_discovery_and_monitoring.py, test_server_selection* — sharded / SDAM
    #   test_csot.py                         — client-side timeouts (RS semantics)
    #   test_async_loop_safety / _unblocked / _cancellation / contextvars —
    #     event-loop-internals tests of pymongo's async machinery, not server
    #     conformance.
]


# Specific async test IDs to deselect — passed to pytest's ``--deselect``
# by the ``validate-pymongo-async`` task. Mirrors
# ``pymongo_validation.include_paths.DESELECT_TESTS``: tests that fail for
# reasons unrelated to SecantusDB compatibility (pymongo-side test bugs,
# xdist/execnet serialization limits). Each carries a one-line reason so the
# next pymongo bump can reassess.
DESELECT_TESTS: list[str] = []
