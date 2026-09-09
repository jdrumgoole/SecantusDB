"""Which vendored psycopg test paths the gauge runs, and which node ids to skip.

Same include/deselect model as ``pymongo_validation``: the suite itself is
NEVER modified — divergence lives here. Grow ``INCLUDE`` as conformance grows;
a deselect needs a one-line reason.
"""

import sys

# Test paths (relative to vendor/psycopg) the gauge measures: every sync
# module, its async twin (test_*_async.py -- the AsyncConnection wire path
# runs in the same lane, under psycopg's own anyio/asyncio plumbing), the
# libpq-level `tests/pq` suite, and the module-level checks. Kept out, each
# with its reason:
# - tests/pool: psycopg_pool is a separate package with its own release line.
# - tests/dns.py, tests/dns_srv.py: need a DNS/SRV resolver fixture, not a
#   database.
# - tests/crdb: CockroachDB-only tests, skipped by their own fixture.
# - tests/test_gevent.py: needs the gevent monkey-patch runner.
# - tests/test_free_threading.py: needs a free-threaded (t) CPython build.
# - tests/test_windows.py: Windows-only event-loop policy test.
INCLUDE = [
    "tests/test_adapt.py",
    "tests/test_capabilities.py",
    "tests/test_column.py",
    "tests/test_concurrency.py",
    "tests/test_concurrency_async.py",
    "tests/test_connection.py",
    "tests/test_connection_async.py",
    "tests/test_connection_info.py",
    "tests/test_conninfo.py",
    "tests/test_conninfo_attempts.py",
    "tests/test_conninfo_attempts_async.py",
    "tests/test_copy.py",
    "tests/test_copy_async.py",
    "tests/test_cursor.py",
    "tests/test_cursor_async.py",
    "tests/test_cursor_client.py",
    "tests/test_cursor_client_async.py",
    "tests/test_cursor_common.py",
    "tests/test_cursor_common_async.py",
    "tests/test_cursor_raw.py",
    "tests/test_cursor_raw_async.py",
    "tests/test_cursor_server.py",
    "tests/test_cursor_server_async.py",
    "tests/test_encodings.py",
    "tests/test_errors.py",
    "tests/test_generators.py",
    "tests/test_module.py",
    "tests/test_notify.py",
    "tests/test_notify_async.py",
    "tests/test_pipeline.py",
    "tests/test_pipeline_async.py",
    "tests/test_prepared.py",
    "tests/test_prepared_async.py",
    "tests/test_psycopg_dbapi20.py",
    "tests/test_query.py",
    "tests/test_rows.py",
    "tests/test_sql.py",
    "tests/test_tpc.py",
    "tests/test_tpc_async.py",
    "tests/test_typeinfo.py",
    "tests/test_typing.py",
    "tests/test_transaction.py",
    "tests/test_transaction_async.py",
    "tests/test_waiting.py",
    "tests/test_waiting_async.py",
    "tests/test_xid.py",
    "tests/pq",
    "tests/types",
]

# t-string syntax (PEP 750) parses only on 3.14+. Upstream's conftest
# collect_ignores the file below that, but an explicitly listed path bypasses
# collect_ignore and errors at collection — so list it under the same gate.
if sys.version_info[:2] >= (3, 14):
    INCLUDE.append("tests/test_tstring.py")

# Individual node ids excluded from the run (NOT counted as failures), each
# with a reason. Prefer fixing the server; deselect only test-infrastructure
# mismatches and hangs.
DESELECT_TESTS: list[str] = [
    # Fails identically against native PostgreSQL 16 (`assert 0 == 160015`,
    # measured 2026-09-09): the test's ctypes `libpq` fixture loads the
    # system libpq, which is not the one psycopg_binary bundles, so
    # `PQserverVersion` on the foreign PGconn pointer answers 0. A harness
    # mismatch, not a server one.
    "tests/pq/test_pgconn.py::test_pgconn_ptr",
]

# A pytest `-m` expression the runner threads through, or None for no marker
# filter. psycopg's own CI excludes these markers on the same platforms
# (vendor/psycopg/.github/workflows/tests.yml).
MARKER_EXPR: str | None = None

# macOS only. All of these are the psycopg test harness, not the server, and
# all reproduce against native PostgreSQL 16 over TCP on this platform
# (measured 2026-09-09):
# - test_generators.py::test_cancel: psycopg's `waiting.wait_conn(gen,
#   interval=0.0)` drives `PQcancelPoll` before the non-blocking loopback
#   connect() has completed; libpq goes STARTED -> MADE on a socket that is
#   not connected yet, its SSLRequest send() fails silently, and it waits in
#   SSL_STARTUP for a byte the server was never asked for. A TCP tap shows the
#   client writes nothing on the cancel socket; the same libpq calls polled
#   every 100 ms pass. Linux loopback connects synchronously, so the race does
#   not exist there and the test stays in the run.
# - the `proxy` marker: the fixture's `_wait_listen` reuses one socket across
#   failed connect_ex() calls, which BSD refuses with EINVAL. psycopg's own CI
#   excludes the marker on macOS and Windows for exactly this.
# - the `timing` marker: these assert wall-clock budgets of a few ms
#   (`test_identify_closure` and friends) that macOS's scheduler does not
#   meet; psycopg's CI excludes the marker on macOS too. test_identify_closure
#   itself passes here when run alone.
if sys.platform == "darwin":
    DESELECT_TESTS += [
        "tests/test_generators.py::test_cancel",
    ]
    MARKER_EXPR = "not proxy and not timing"
