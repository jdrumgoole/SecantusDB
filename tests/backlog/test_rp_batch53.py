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
import pytest

from tests.backlog.conftest import pg_answer

_LIST = ",".join(str(i) for i in range(200_000))


@pytest.mark.backlog_fixed("RP-215")
@pytest.mark.parametrize("delay", [0.5, 1.5, 2.5])
def test_a_cancel_during_a_prepared_statements_parse_is_honoured(pg: Any, delay: float) -> None:
    """psycopg's `prepare=True` sends Parse and Sync as a cycle of their own.
    A cancel that landed while the Parse was being described was left
    pending when the Parse answered, cleared as stale at the next message,
    and the statement ran to its end -- `pg_sleep(3)` included, 6 to 8 s
    later. With user code present the statement plans as one transaction,
    the slower shape RP-002's tests use."""
    pg.execute("create function rp053_upper(int) returns int language sql as 'select 1'")
    pg.execute("create function upper(int) returns int language sql as 'select 1'")
    try:
        # A different statement each time: none is planned from a cache.
        sql = f"select pg_sleep(3), {int(delay * 10)} in ({_LIST})"
        timer = threading.Timer(delay, pg.cancel)
        t0 = time.monotonic()
        timer.start()
        try:
            pg.execute(sql, prepare=True)
            answer: tuple[str, str] = ("ok", "")
        except psycopg.Error as e:
            answer = ("err", e.sqlstate or "")
        elapsed = time.monotonic() - t0
        timer.join()
        assert answer == ("err", "57014")
        assert elapsed - delay < 0.5, f"cancel honoured {elapsed - delay:.2f} s after it was sent"
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
