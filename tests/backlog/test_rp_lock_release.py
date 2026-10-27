"""Lock release as PostgreSQL does it (2026-10-27): found chasing
`test_rp_isolation.py`'s CI flake.

RP-270: PostgreSQL's releaser grants a waiter its lock before the holder's
COMMIT answers, so once it has, `pg_isolation_test_session_is_blocked`
(and `pg_blocking_pids`) no longer name the holder. This server's waiter
cleared its own wait-for edge on its next poll, and the isolation tester's
question, asked right after the COMMIT, saw it still blocked: the tester
then sent the waiting session its next step, and psycopg refused it
("another command is already in progress").

RP-271: an autocommit statement took no table lock. An autocommit UPDATE
waiting on a row and a DROP TABLE of the same table both went on when the
row's holder committed; the drop held the storage's locks while it waited
out the update's uncommitted write, which waited for those locks, and the
server never answered again -- or, as the two raced, the drop went first
and the UPDATE answered `UPDATE 0`. PostgreSQL's DROP waits for the
UPDATE's ROW EXCLUSIVE lock, then drops.

Both measured on PostgreSQL 15.19 (port 5415), the same client code.
"""

from __future__ import annotations

import time
from pathlib import Path
from typing import Any

import psycopg
import pytest

BLOCKED = "select pg_isolation_test_session_is_blocked(%s, %s::int4[])"


def _wait_blocked(ctl: Any, pid: int, others: list[int]) -> None:
    deadline = time.monotonic() + 10
    while not ctl.execute(BLOCKED, (pid, others)).fetchone()[0]:
        assert time.monotonic() < deadline, "the second insert never waited"
        time.sleep(0.005)


def _drain(conn: Any, seconds: float) -> list[tuple[str, str]]:
    """The results of the query `conn` sent, or `[("timeout", "")]`."""
    pg = conn.pgconn
    deadline = time.monotonic() + seconds
    while True:
        pg.consume_input()
        if not pg.is_busy():
            break
        if time.monotonic() >= deadline:
            return [("timeout", "")]
        time.sleep(0.01)
    out = []
    while (r := pg.get_result()) is not None:
        if r.status == psycopg.pq.ExecStatus.FATAL_ERROR:
            out.append(
                ("error", (r.error_field(psycopg.pq.DiagnosticField.SQLSTATE) or b"").decode())
            )
        else:
            out.append(("ok", (r.command_status or b"").decode()))
    return out


@pytest.mark.backlog_fixed("RP-270")
def test_a_waiter_is_not_blocked_once_the_holders_commit_answers(rust_pg: Any) -> None:
    """PostgreSQL 15.19: a second insert of an uncommitted key waits; the
    moment the first transaction's COMMIT has answered, the second session
    is no longer blocked -- 0 of 50 times reported blocked. This server: 48
    of 50."""
    with rust_pg.connect() as ctl:
        ctl.execute("create table rp270_t (k int primary key)")
        stale = 0
        try:
            for _ in range(20):
                ctl.execute("delete from rp270_t")
                with rust_pg.connect() as s1, rust_pg.connect() as s2:
                    p1 = s1.execute("select pg_backend_pid()").fetchone()[0]
                    p2 = s2.execute("select pg_backend_pid()").fetchone()[0]
                    s1.execute("begin")
                    s1.execute("insert into rp270_t values (1)")
                    s2.pgconn.send_query(b"insert into rp270_t values (1)")
                    _wait_blocked(ctl, p2, [p1])
                    s1.execute("commit")
                    if ctl.execute(BLOCKED, (p2, [p1])).fetchone()[0]:
                        stale += 1
                    assert _drain(s2, 30) == [("error", "23505")]
        finally:
            ctl.execute("drop table rp270_t")
    assert stale == 0, f"still reported blocked after the holder's COMMIT, {stale} of 20"


@pytest.mark.backlog_fixed("RP-271")
def test_a_drop_waits_for_an_autocommit_update(tmp_path: Path) -> None:
    """PostgreSQL 15.19: session 1 holds row 1 (an ON CONFLICT DO UPDATE
    whose WHERE rejects it); session 2's autocommit UPDATE of it waits; a
    DROP TABLE waits too. Session 1 commits: the UPDATE answers `UPDATE 1`,
    then the DROP `DROP TABLE` (10 of 10). This server, as the two raced:
    wedged for good (a probe's first try), or dropped the table under the
    UPDATE, which answered `UPDATE 0`. The test runs its own server, so a
    wedge fails this test alone."""
    from tests.test_rust_pgserver_slice import _Server

    with _Server(tmp_path) as server:
        with server.connect() as ctl:
            ctl.execute("create table rp271_t (key int primary key, val int)")
            ctl.execute("insert into rp271_t values (1, 0)")
        s1 = server.connect()
        s2 = server.connect()
        d = server.connect()
        try:
            s1.execute("begin")
            s1.execute(
                "insert into rp271_t values (1, 99) on conflict (key) "
                "do update set val = 99 where rp271_t.val > 100"
            )
            # The simple protocol, as `psql` and the isolation tester send it:
            # one statement outside any transaction.
            s2.pgconn.send_query(b"update rp271_t set val = val + 1 where key = 1")
            time.sleep(0.2)
            d.pgconn.send_query(b"drop table rp271_t")
            time.sleep(0.2)
            s1.execute("commit")
            answers = (_drain(s2, 15), _drain(d, 15))
        finally:
            for c in (s1, s2, d):
                c.close()
    assert answers == ([("ok", "UPDATE 1")], [("ok", "DROP TABLE")])
