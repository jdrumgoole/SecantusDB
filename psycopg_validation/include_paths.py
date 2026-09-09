"""Which vendored psycopg test paths the gauge runs, and which node ids to skip.

Same include/deselect model as ``pymongo_validation``: the suite itself is
NEVER modified — divergence lives here. Grow ``INCLUDE`` as conformance grows;
a deselect needs a one-line reason.
"""

import sys

# Test paths (relative to vendor/psycopg) the gauge currently measures. The
# sync half of the suite: the async twins (test_*_async.py) need the
# AsyncConnection wire path measured separately (a later lane, like
# pymongo-async). Excluded wholesale for now: pool tests (psycopg_pool is a
# separate package), dns/srv (needs a resolver fixture), tpc (two-phase against
# an external transaction manager), replication/large-objects (server features
# out of scope).
INCLUDE = [
    "tests/test_adapt.py",
    "tests/test_capabilities.py",
    "tests/test_column.py",
    "tests/test_connection.py",
    "tests/test_connection_info.py",
    "tests/test_conninfo.py",
    "tests/test_copy.py",
    "tests/test_cursor.py",
    "tests/test_cursor_client.py",
    "tests/test_cursor_common.py",
    "tests/test_cursor_raw.py",
    "tests/test_cursor_server.py",
    "tests/test_encodings.py",
    "tests/test_errors.py",
    "tests/test_generators.py",
    "tests/test_prepared.py",
    "tests/test_psycopg_dbapi20.py",
    "tests/test_query.py",
    "tests/test_rows.py",
    "tests/test_sql.py",
    "tests/test_tstring.py",
    "tests/test_typeinfo.py",
    "tests/test_typing.py",
    "tests/test_transaction.py",
    "tests/types",
]

# Individual node ids excluded from the run (NOT counted as failures), each
# with a reason. Prefer fixing the server; deselect only test-infrastructure
# mismatches and hangs.
DESELECT_TESTS: list[str] = []

# macOS only. Both are the psycopg test harness, not the server, and both
# reproduce against native PostgreSQL 16 over TCP on this platform (measured
# 2026-09-09):
# - test_generators.py::test_cancel: psycopg's `waiting.wait_conn(gen,
#   interval=0.0)` drives `PQcancelPoll` before the non-blocking loopback
#   connect() has completed; libpq goes STARTED -> MADE on a socket that is
#   not connected yet, its SSLRequest send() fails silently, and it waits in
#   SSL_STARTUP for a byte the server was never asked for. A TCP tap shows the
#   client writes nothing on the cancel socket; the same libpq calls polled
#   every 100 ms pass. Linux loopback connects synchronously, so the race does
#   not exist there and the test stays in the run.
# - test_connection.py::test_cancel_safe_*: the `proxy` fixture's
#   `_wait_listen` reuses one socket across failed connect_ex() calls, which
#   BSD refuses with EINVAL — psycopg's own CI excludes the `proxy` marker on
#   macOS and Windows for this reason (vendor/psycopg/.github/workflows/tests.yml).
if sys.platform == "darwin":
    DESELECT_TESTS += [
        "tests/test_generators.py::test_cancel",
        "tests/test_connection.py::test_cancel_safe_error",
        "tests/test_connection.py::test_cancel_safe_timeout",
    ]
