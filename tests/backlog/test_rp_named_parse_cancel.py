"""A cancel during a NAMED statement's Parse (RP-303).

PostgreSQL 15.19 checks for interrupts throughout `exec_parse_message`: a
cancel that arrives while a Parse is still being worked answers that Parse
with 57014, and the statement is not stored. This server, after the Parse's
analysis, built a named statement's `pg_prepared_statements` row -- a second
scan and describe of the statement -- with no cancellation point. A cancel
landing then was still pending when the Parse answered; psycopg's
`prepare=True` sends Parse and Sync as a cycle of their own, so the Sync
closed the cycle and the Bind's first byte cleared the cancel as stale. The
statement ran to its end. That was RP-215's regression test answering `ok`
nine times under load, where the Parse of its 200,000-item statement ran
close to the cancel's 2.5 s.

An `EXPLAIN` is analysed at Execute here, so its named Parse spends most of
its time on that row: the gap between a named and an unnamed Parse of the
same text is the window, and the cancel is aimed at its middle.
"""

from __future__ import annotations

import threading
import time
from typing import Any

import psycopg.pq as pq
import pytest

# The list's length sets the window. 200,000 items gave a debug build
# 0.65 s locally but 0.19 s on a CI runner (PR #2395), so the test doubles it
# until the window is wide enough to aim at.
_SIZES = (200_000, 400_000, 800_000)
_WINDOW = 0.3
# How long before a Parse answers its cancel must leave to be sure of
# reaching it: the cancel opens a connection of its own. PostgreSQL 15.19
# answers a cancelled Parse about 0.015 s after the cancel is sent.
_ARRIVAL = 0.1


def _sql(n: int, size: int) -> bytes:
    # A different text each time: none is parsed from the planner's memo.
    items = ",".join(str(i) for i in range(size))
    return f"explain select pg_sleep(3), {n} in ({items})".encode()


@pytest.mark.backlog_fixed("RP-303")
def test_a_cancel_while_a_named_parse_records_its_statement_is_the_parses_57014(
    pg: Any,
) -> None:
    """Measured against PostgreSQL 15.19 (RP-215): a cancel during a Parse is
    that Parse's 57014, about 0.015 s after it is sent. Before the fix, a
    cancel sent between the unnamed and the named Parse's durations answered
    `ParseComplete` and was lost."""
    pgconn = pg.pgconn

    def timed(name: bytes, n: int, size: int) -> float:
        t0 = time.monotonic()
        result = pgconn.prepare(name, _sql(n, size))
        assert result.status == pq.ExecStatus.COMMAND_OK, result.error_message
        return time.monotonic() - t0

    n = 0
    windows = []
    for size in _SIZES:
        unnamed = min(timed(b"", n := n + 1, size) for _ in range(2))
        named = min(timed(b"rp303_calibrate_%d" % (n := n + 1), n, size) for _ in range(2))
        windows.append(f"{size}: {unnamed:.3f}s / {named:.3f}s")
        # The row's own work, which the cancel must land in; a debug build
        # measured 1.09 s and 1.74 s at 200,000 items.
        if named - unnamed > _WINDOW:
            break
    assert named - unnamed > _WINDOW, f"no window to aim at: {windows}"
    delay = (unnamed + named) / 2

    sent: dict[str, float] = {}

    def cancel() -> None:
        sent["at"] = time.monotonic()
        pg.cancel()

    # On a slow CI runner the target Parse has answered BEFORE a cancel aimed
    # at the middle of the calibrated window left (PR #2397: 0.27 s before),
    # and 0.007 s AFTER it left (PR #2402), sooner than a cancel's own new
    # connection reaches the server. Neither trial tests anything -- the
    # cancel never met the Parse -- so each is aimed again, at the middle of
    # the window that Parse showed. A cancel that left _ARRIVAL or more
    # before the Parse answered, and is lost, still fails below.
    window_end = named
    missed = []
    for attempt in range(4):
        target = b"rp303_target_%d" % attempt
        timer = threading.Timer(delay, cancel)
        t0 = time.monotonic()
        timer.start()
        result = pgconn.prepare(target, _sql(n := n + 1, size))
        answered = time.monotonic()
        timer.join()
        cancelled = result.status == pq.ExecStatus.FATAL_ERROR
        if cancelled or answered - sent["at"] >= _ARRIVAL:
            break
        window_end = answered - t0
        missed.append(
            f"aimed at {delay:.3f}s, the Parse answered at {window_end:.3f}s, "
            f"{answered - sent['at']:+.3f}s after the cancel left"
        )
        delay = (unnamed + window_end) / 2
        # The late cancel reaches an idle session, where PostgreSQL ignores
        # it. A pause and a round trip let it arrive before the next trial,
        # so it cannot cancel that trial's Parse and pass for its own.
        time.sleep(0.5)
        pgconn.exec_(b"select 1")
    else:
        pytest.fail(f"no cancel left in time to meet its Parse: {missed}")
    assert result.status == pq.ExecStatus.FATAL_ERROR, (
        f"the Parse answered {pq.ExecStatus(result.status).name} "
        f"{answered - sent['at']:.3f}s after the cancel was sent"
    )
    assert result.error_field(pq.DiagnosticField.SQLSTATE) == b"57014"
    assert answered - sent["at"] < 0.5, (
        f"cancel honoured {answered - sent['at']:.2f}s after it was sent"
    )
    # The window's end is a measurement, not a deadline: a cancel honoured in
    # its last moments cannot be told from a Parse that finished.
    assert answered - t0 < window_end + _ARRIVAL, "the Parse was not cut short"
    # The cancelled Parse stored nothing, as PostgreSQL's does not.
    row = pg.execute(
        "select count(*) from pg_prepared_statements where name = %s",
        (target.decode(),),
    ).fetchone()
    assert row == (0,)
