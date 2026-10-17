"""Gap test for `tasks/backlog-rust-postgres.md` RP-048: "Open from the
lock-manager work (2026-10-17)".

The expected value is a literal: PostgreSQL's deadlock detector is one
wait-for graph over every lock kind, so a cycle is `40P01` in one waiter
however it runs. The same-type cycles (row waits, and advisory locks since
d5f9de1d) were measured live; the cross-kind case is the same detector, and
a committed table on the shared reference is what the reference-protection
rule forbids.
"""

from __future__ import annotations

import contextlib
import threading
import time
from typing import Any

import psycopg
import pytest


@pytest.mark.backlog("RP-048")
def test_a_cycle_across_lock_kinds_is_a_deadlock(rust_pg: Any) -> None:
    """PostgreSQL 15.19: the deadlock detector is one wait-for graph over
    every lock kind, so a cycle that runs through a row lock and an advisory
    lock is `40P01` in one waiter (its other holds survive; the other
    session's wait clears once the victim's block ends). Measured here: both
    sessions instead wait out `lock_timeout` -- neither wait's own graph can
    see the other kind's edge."""
    with rust_pg.connect() as setup, rust_pg.connect() as a, rust_pg.connect() as b:
        setup.execute("create table rp048_t (id int primary key, n int)")
        setup.execute("insert into rp048_t values (1, 0)")
        a.execute("begin")
        a.execute("select pg_advisory_lock(717171)")  # A holds the advisory lock
        b.execute("begin")
        b.execute("update rp048_t set n = 2 where id = 1")  # B holds the row
        a.execute("set lock_timeout = '3s'")
        b.execute("set lock_timeout = '3s'")
        outcome: dict[str, str] = {}

        def side(name: str, conn: Any, waiting_sql: str) -> None:
            try:
                conn.execute(waiting_sql)
                outcome[name] = "granted"
            except psycopg.Error as e:
                outcome[name] = e.sqlstate
                conn.execute("rollback")  # let the survivor's wait clear

        ta = threading.Thread(target=side, args=("a", a, "update rp048_t set n = 3 where id = 1"))
        ta.start()
        time.sleep(0.3)
        tb = threading.Thread(target=side, args=("b", b, "select pg_advisory_lock(717171)"))
        tb.start()
        ta.join(15)
        tb.join(15)
        assert sorted(outcome.values()) == ["40P01", "granted"], outcome
        for conn in (a, b):
            with contextlib.suppress(psycopg.Error):
                conn.execute("rollback")
