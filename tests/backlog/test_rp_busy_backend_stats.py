"""A backend's held statistics are published while it is busy again.

PostgreSQL 15 holds the counts a backend could not flush yet (its last
flush under a second before) for the idle timer, and turns that timer off
as soon as the next command is read: the counts go out when the backend is
next idle. Here `begin_message_cycle`'s wake never runs -- the wire layer's
`MessageStartHook` opens the cycle at the message's first byte, so the
branch that ends the idle wait is dead -- and another backend's report
flushes them ten seconds later, mid-statement.
"""

from __future__ import annotations

import threading
import time
from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


@pytest.mark.backlog("RP-1116")
def test_a_busy_backends_held_counts_wait_until_it_is_idle(rust_pg: Any) -> None:
    """Measured against PostgreSQL 15.20: the second insert's count, held
    for the idle timer, is not visible while its backend sleeps (1), and is
    once the backend is idle (2)."""
    with rust_pg.connect() as a, rust_pg.connect() as b:
        a.execute("create table rp_busy_stats(x int)")
        try:
            time.sleep(1.2)
            a.execute("insert into rp_busy_stats values (1)")  # flushed at once
            a.execute("insert into rp_busy_stats values (2)")  # held: under 1 s since
            sleeper = threading.Thread(target=lambda: a.execute("select pg_sleep(13)"))
            sleeper.start()
            time.sleep(11.5)  # past the 10 s idle timer the backend no longer runs
            b.execute("select 1")  # a report of B's own
            count = "select n_tup_ins from pg_stat_user_tables where relname = 'rp_busy_stats'"
            during = pg_answer(b, count)
            sleeper.join()
            after = pg_answer(b, count)
            assert (during, after) == (("ok", [(1,)]), ("ok", [(2,)]))
        finally:
            a.execute("drop table rp_busy_stats")
