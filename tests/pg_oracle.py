"""One way to reach the local PostgreSQL reference server, for every suite
that diffs against it.

Six suites grew their own copy of "connect, swallow the exception, return
None". The copies had drifted -- three different default DSNs, one of which
omitted the user -- and every one of them skipped with a reason that said
nothing.

**The skip reason was the defect.** ``no local PostgreSQL oracle`` on its own
is indistinguishable from *PostgreSQL is not installed*, so a suite disabled by
a connection failure looks exactly like one switched off on purpose. A backlog
entry recorded these ~109 tests as silently skipping in the full suite; a full
run on 2026-08-31 shows all 195 of them executing, but answering that took a
20-minute suite run because the skip line carried no information either way.
Every skip from this module now names the DSN **and** the exception.

**One probe per worker, not nine.** These helpers are called from `skipif`,
which is evaluated once per decorator at import time, on every xdist worker.
Across the six suites that was nine connects per worker --
``test_sql_subms_timestamps.py`` alone had five. `available()` caches its
verdict at module scope, so a worker now pays for one.

That matters because of the failure mode below: when a connect *hangs* rather
than fails, the old shape cost ``9 x connect_timeout`` of dead collection time
per worker, and the new one costs one timeout.

**Measured, so the next session does not re-derive it: the old shape did NOT
leak connections** (2026-08-31). It looks like it must -- ``_pg_oracle() is
None`` drops the connection unclosed -- but CPython refcounting collects it the
moment the comparison is computed, and psycopg closes it on `__del__`. Probed
against this box's PostgreSQL: nine old-shape probes left **zero** connections
in `pg_stat_activity`, where nine probes whose references are *held* leave
nine. Connection exhaustion was therefore never what disabled these suites, and
the earlier diagnosis that ruled it out by measurement was right.

**One cause worth recognising: Postgres.app's permission gate.** This box runs
Homebrew's PostgreSQL 14, which has no such gate -- but if
`SECANTUS_PG_ORACLE_DSN` ever points at Postgres.app, it gates connections per
application behind a macOS dialog. An unapproved process -- every pytest-xdist
worker -- waits on a dialog nothing can answer until the timeout expires,
surfacing as a bare `ConnectionTimeout`:

    FATAL:  Postgres.app failed to verify "trust" authentication
    DETAIL:  You did not confirm the permission dialog.

If the skip reason from this module shows that, fix it in Postgres.app's
settings or point `SECANTUS_PG_ORACLE_DSN` at a plain PostgreSQL -- there is
nothing to fix in the test. The point of naming the exception is that this is
now visible in the pytest output instead of being guessed at.
"""

from __future__ import annotations

import os
import time
from typing import Any

#: Where the reference server lives. Override for a non-default PostgreSQL.
DEFAULT_DSN = "host=127.0.0.1 port=5432 dbname=postgres user=jdrumgoole"

# Short and few on purpose. The failure mode is a HANG, not a blip: an
# unapproved process waits on a permission dialog until the timeout expires, so
# a generous budget buys nothing and costs that budget per worker.
CONNECT_TIMEOUT_S = 5
CONNECT_ATTEMPTS = 2

#: Why the last attempt failed, for the skip reason. A bare "no oracle" is what
#: hid this for months.
_last_error = "never attempted"

#: Cached verdict from `available()`, so N decorators cost ONE connection.
_available: bool | None = None


def dsn() -> str:
    """The DSN to reach the reference server."""
    return os.environ.get("SECANTUS_PG_ORACLE_DSN", DEFAULT_DSN)


def connect() -> Any | None:
    """Connect to the reference server, retrying once, or return None.

    The caller owns the connection and must close it. For a yes/no check use
    `available()`, which does not leak.
    """
    global _last_error
    try:
        import psycopg
    except ImportError as exc:
        _last_error = f"{type(exc).__name__}: {exc}"
        return None

    delay = 0.5
    for attempt in range(CONNECT_ATTEMPTS):
        try:
            return psycopg.connect(dsn(), autocommit=True, connect_timeout=CONNECT_TIMEOUT_S)
        except Exception as exc:  # noqa: BLE001 - retry, then record why
            _last_error = f"{type(exc).__name__}: {exc}"
            if attempt == CONNECT_ATTEMPTS - 1:
                return None
            time.sleep(delay)
            delay *= 2
    return None


def available() -> bool:
    """Is the reference server reachable? Closes its probe; caches the answer.

    Cached because this is called from `skipif`, which is evaluated once per
    decorator at import time. Without the cache a module with five decorators
    opened five connections on every xdist worker.
    """
    global _available
    if _available is None:
        conn = connect()
        if conn is None:
            _available = False
        else:
            conn.close()
            _available = True
    return _available


def skip_reason() -> str:
    """Why the oracle suites are skipping, in enough detail to act on."""
    return f"no local PostgreSQL reference server ({dsn()}): {_last_error}"
