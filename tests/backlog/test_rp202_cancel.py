"""Cancels the top-ten batch (2026-10-26) made prompt.

PostgreSQL 15.19 answers a cancel at its next `CHECK_FOR_INTERRUPTS`, about
0.015 s after it is sent, whatever the statement is doing: planning a
200,000-item `IN` list (RP-202) or sleeping in a row's select list
(RP-286). The budgets here are wide, for a debug build on a shared runner;
the measurements they guard are in the RP-202 changelog fragment.
"""

from __future__ import annotations

import threading
import time
from typing import Any

import psycopg
import pytest

_LIST = ",".join(str(i) for i in range(200_000))


def _cancel_after(conn: Any, delay: float, sql: str) -> tuple[str, float]:
    """Send `sql` (prepared), cancel it `delay` seconds in: the answer and
    how long after the cancel it came."""
    timer = threading.Timer(delay, conn.cancel)
    t0 = time.monotonic()
    timer.start()
    try:
        conn.execute(sql, prepare=True).fetchall()
        state = "ok"
    except psycopg.Error as e:
        state = e.sqlstate or ""
    elapsed = time.monotonic() - t0
    timer.join()
    return state, elapsed - delay


@pytest.mark.backlog_fixed("RP-286")
def test_a_sleep_in_a_streamed_select_list_is_cancelled(rust_pg: Any) -> None:
    """`select a, pg_sleep(2) from t`: the row is streamed, and its select
    list is evaluated after the statement's hooks are gone. The sleep did not
    look for a cancel and ran its two seconds."""
    with rust_pg.connect() as conn:
        conn.execute("create table if not exists rp286_t (a int)")
        conn.execute("insert into rp286_t values (1)")
        state, late = _cancel_after(conn, 0.5, "select a, pg_sleep(2) from rp286_t")
        conn.execute("drop table rp286_t")
    assert state == "57014"
    assert late < 0.5, f"cancel honoured {late:.2f} s after it was sent"


@pytest.mark.backlog_fixed("RP-202")
@pytest.mark.parametrize("delay", [0.1, 0.4, 0.7, 1.0])
def test_a_cancel_while_a_huge_in_list_is_planned_over_a_table(rust_pg: Any, delay: float) -> None:
    """Planning a 200,000-item `IN` list over a table runs a sequence of
    rewrite passes; each copied the statement, and a copy cannot be
    interrupted. A cancel waited for the pass it landed in."""
    with rust_pg.connect() as conn:
        conn.execute("create table if not exists rp202_t (a int, b text)")
        state, late = _cancel_after(
            conn,
            delay,
            f"select a, b, pg_sleep(3) from rp202_t where a in ({_LIST}) and b <> '{delay}'",
        )
    assert state == "57014"
    assert late < 0.3, f"cancel honoured {late:.2f} s after it was sent"
