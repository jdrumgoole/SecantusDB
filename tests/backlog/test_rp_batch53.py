"""Batch 53 of the Rust PG server (2026-10-24): the regression tests of
RP-215 (a cancel during an extended-protocol Parse) and RP-216 (a constant
IN list evaluated in quadratic time).

PostgreSQL 15.19 answers a cancel at its next `CHECK_FOR_INTERRUPTS`,
0.015 s after it is sent, in whichever message of the cycle it lands; it
evaluates `x IN (list)` in time linear in the list.
"""

from __future__ import annotations

import threading
import time
from typing import Any

import psycopg
import psycopg.pq as pq
import pytest

from tests.backlog.conftest import cancel_trace_window, pg_answer

_LIST = ",".join(str(i) for i in range(200_000))

# A cancel that misses prints the server's cancel trace: the misses come
# from CI under load, not on demand (RP-1115).
CANCEL_TRACE = True


# Where in the Parse each cancel is aimed, as a fraction of the Parse cycle
# measured just before it: the first in the parse and analysis, the others
# in the describe pass, which is most of the cycle.
_FRACTIONS = (0.2, 0.45, 0.7)
# How long before the Parse answers its cancel must be sent to be sure of
# reaching it: the cancel opens a connection of its own.
_ARRIVAL = 0.1


@pytest.mark.backlog_fixed("RP-215")
@pytest.mark.parametrize("fraction", _FRACTIONS)
def test_a_cancel_during_a_prepared_statements_parse_is_honoured(
    pg: Any, rust_pg: Any, fraction: float
) -> None:
    """psycopg's `prepare=True` sends Parse and Sync as a cycle of their own.
    A cancel that landed while the Parse was being described was left
    pending when the Parse answered, cleared as stale at the next message,
    and the statement ran to its end -- `pg_sleep(3)` included, 6 to 8 s
    later. With user code present the statement plans as one transaction,
    the slower shape RP-002's tests use.

    Each cancel is aimed inside the Parse as this machine runs it. The test
    once cancelled at fixed times (0.5, 1.5 and 2.5 s): on a CI runner whose
    Parse cycle ended near 2.5 s, that cancel landed while the backend was
    idle between the Parse's ReadyForQuery and the Bind, where PostgreSQL
    15.20 drops a cancel too (`PostgresMain` clears `QueryCancelPending`
    after reading the next command), and the statement ran (RP-1115); on a
    fast machine it landed in the Execute and tested no Parse at all."""
    pg.execute("create function rp053_upper(int) returns int language sql as 'select 1'")
    pg.execute("create function upper(int) returns int language sql as 'select 1'")
    try:
        # The Parse cycle of the same shape of statement, a different text:
        # none is parsed or planned from a cache.
        n = int(fraction * 100)
        t0 = time.monotonic()
        result = pg.pgconn.prepare(
            b"rp053_calibrate", f"select pg_sleep(3), -{n} in ({_LIST})".encode()
        )
        parse = time.monotonic() - t0
        assert result.status == pq.ExecStatus.COMMAND_OK, result.error_message
        pg.execute("deallocate rp053_calibrate")
        delay = min(fraction * parse, parse - _ARRIVAL)
        assert delay > 0, f"a Parse of {parse:.3f} s leaves no time to cancel in"

        sql = f"select pg_sleep(3), {n} in ({_LIST})"
        timer = threading.Timer(delay, pg.cancel)
        wall = time.time()
        t0 = time.monotonic()
        timer.start()
        try:
            pg.execute(sql, prepare=True)
            answer: tuple[str, str] = ("ok", "")
        except psycopg.Error as e:
            answer = ("err", e.sqlstate or "")
        elapsed = time.monotonic() - t0
        timer.join()
        context = (
            f"cancel {delay:.3f} s into a Parse measured at {parse:.3f} s"
            + cancel_trace_window(rust_pg, wall, time.time())
        )
        assert answer == ("err", "57014"), context
        assert elapsed - delay < 0.5, (
            f"cancel honoured {elapsed - delay:.2f} s after it was sent; {context}"
        )
    finally:
        pg.execute("drop function rp053_upper(int)")
        pg.execute("drop function upper(int)")


@pytest.mark.backlog_fixed("RP-216")
def test_a_long_constant_in_list_matching_late_is_linear(pg: Any) -> None:
    """Each item of a constant `x IN (...)` was compared by cloning the
    whole expression, list included: a 200,000-item list matching at its
    780th item took 24 s, at its last it would have taken hours. Bounded by
    `statement_timeout`, so the old behaviour fails here rather than hangs."""
    pg.execute("set statement_timeout = '120s'")
    try:
        assert pg_answer(pg, f"select 199999 in ({_LIST}), -5 in ({_LIST})") == (
            "ok",
            [(True, False)],
        )
    finally:
        pg.execute("reset statement_timeout")
