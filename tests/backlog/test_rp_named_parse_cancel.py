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

_LIST = ",".join(str(i) for i in range(200_000))


def _sql(n: int) -> bytes:
    # A different text each time: none is parsed from the planner's memo.
    return f"explain select pg_sleep(3), {n} in ({_LIST})".encode()


@pytest.mark.backlog_fixed("RP-303")
def test_a_cancel_while_a_named_parse_records_its_statement_is_the_parses_57014(
    pg: Any,
) -> None:
    """Measured against PostgreSQL 15.19 (RP-215): a cancel during a Parse is
    that Parse's 57014, about 0.015 s after it is sent. Before the fix, a
    cancel sent between the unnamed and the named Parse's durations answered
    `ParseComplete` and was lost."""
    pgconn = pg.pgconn

    def timed(name: bytes, n: int) -> float:
        t0 = time.monotonic()
        result = pgconn.prepare(name, _sql(n))
        assert result.status == pq.ExecStatus.COMMAND_OK, result.error_message
        return time.monotonic() - t0

    unnamed = min(timed(b"", n) for n in (1, 2))
    named = min(timed(b"rp303_calibrate_%d" % n, n) for n in (3, 4))
    # The row's own work, which the cancel must land in; a debug build
    # measured 1.09 s and 1.74 s.
    assert named - unnamed > 0.2, f"no window to aim at: {unnamed:.3f}s / {named:.3f}s"
    delay = (unnamed + named) / 2

    sent: dict[str, float] = {}

    def cancel() -> None:
        sent["at"] = time.monotonic()
        pg.cancel()

    timer = threading.Timer(delay, cancel)
    t0 = time.monotonic()
    timer.start()
    result = pgconn.prepare(b"rp303_target", _sql(5))
    answered = time.monotonic()
    timer.join()
    assert sent["at"] < answered, "the cancel left after the Parse answered"
    assert result.status == pq.ExecStatus.FATAL_ERROR, (
        f"the Parse answered {pq.ExecStatus(result.status).name} "
        f"{answered - sent['at']:.3f}s after the cancel was sent"
    )
    assert result.error_field(pq.DiagnosticField.SQLSTATE) == b"57014"
    assert answered - sent["at"] < 0.5, (
        f"cancel honoured {answered - sent['at']:.2f}s after it was sent"
    )
    assert answered - t0 < named, "the Parse was not cut short"
    # The cancelled Parse stored nothing, as PostgreSQL's does not.
    row = pg.execute(
        "select count(*) from pg_prepared_statements where name = 'rp303_target'"
    ).fetchone()
    assert row == (0,)
