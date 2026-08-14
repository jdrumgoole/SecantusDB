"""Query cancellation: the wire CancelRequest sub-protocol and pg_cancel_backend.

A CancelRequest arrives on its own fresh connection carrying the (pid, secret)
from BackendKeyData; the server fires the target session's cancel_event, which
cancellation points — ``pg_sleep``, per-row or FROM-less — observe and turn
into PG's ``57014 canceling statement due to user request``. The canceled
connection stays fully usable (cancel is not terminate). pgx's cancel cluster
(TestConnCancelRequest / TestConnContextCanceledCancelsRunningQueryOnServer /
TestConnCopyToCanceled / TestCancelRequestContextWatcherHandler) rides exactly
this machinery.

pg_stat_activity reports an extended-protocol statement's ORIGINAL text with
``$1`` placeholders intact, like real PG — the bound render would inline
parameter values, making pgx's ``query like $1`` liveness poll match its own
row forever (and leaking parameter values).
"""

from __future__ import annotations

import threading
import time

import psycopg
import pytest

from secantus.sql.pgserver import SecantusPGServer


@pytest.fixture()
def server(tmp_path):
    srv = SecantusPGServer(storage_path=str(tmp_path), port=0)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture()
def dsn(server):
    host, port = server.address
    return f"host={host} port={port} dbname=test user=test password=test"


def _cancel_after(conn: psycopg.Connection, delay: float) -> threading.Thread:
    # cancel_safe, not the deprecated cancel(): the blocking PQcancel holds
    # the GIL for its whole connect-and-wait, which freezes an IN-PROCESS
    # server's accept loop — the cancel connection is never serviced and the
    # test deadlocks. (A daemon-subprocess server doesn't care.)
    t = threading.Thread(target=lambda: (time.sleep(delay), conn.cancel_safe()))
    t.start()
    return t


class TestWireCancelRequest:
    def test_cancel_interrupts_pg_sleep_and_connection_survives(self, dsn):
        with psycopg.connect(dsn, autocommit=True) as c:
            t = _cancel_after(c, 0.3)
            with pytest.raises(psycopg.errors.QueryCanceled):
                c.execute("select pg_sleep(20)")
            t.join()
            assert c.execute("select 1").fetchone() == (1,)

    def test_cancel_interrupts_per_row_pg_sleep(self, dsn):
        with psycopg.connect(dsn, autocommit=True) as c:
            t = _cancel_after(c, 0.3)
            with pytest.raises(psycopg.errors.QueryCanceled):
                c.execute("select n, pg_sleep(0.05) from generate_series(1, 200) n")
            t.join()
            assert c.execute("select 1").fetchone() == (1,)

    def test_cancel_while_idle_is_ignored(self, dsn):
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("select 1")
            c.cancel_safe()
            time.sleep(0.2)
            # The stale cancel must not fire on the next statement's sleep.
            c.execute("select pg_sleep(0.1)")
            assert c.execute("select 1").fetchone() == (1,)

    def test_cancel_with_wrong_secret_is_ignored(self, dsn, server):
        import socket
        import struct

        with psycopg.connect(dsn, autocommit=True) as c:
            pid = c.execute("select pg_backend_pid()").fetchone()[0]
            host, port = server.address
            with socket.create_connection((host, port), timeout=5) as s:
                s.sendall(struct.pack("!iiii", 16, 80877102, pid, 12345))
            done = []
            t = threading.Thread(
                target=lambda: done.append(c.execute("select pg_sleep(0.5)") and True)
            )
            t.start()
            t.join(timeout=10)
            assert done  # the sleep ran to completion — bogus cancel ignored


class TestPgCancelBackend:
    def test_cancels_other_backend_and_keeps_it_alive(self, dsn):
        with (
            psycopg.connect(dsn, autocommit=True) as victim,
            psycopg.connect(dsn, autocommit=True) as killer,
        ):
            pid = victim.execute("select pg_backend_pid()").fetchone()[0]

            result = []

            def run():
                try:
                    victim.execute("select pg_sleep(20)")
                    result.append("completed")
                except psycopg.errors.QueryCanceled:
                    result.append("canceled")

            t = threading.Thread(target=run)
            t.start()
            time.sleep(0.3)
            assert killer.execute("select pg_cancel_backend(%s)", (pid,)).fetchone() == (True,)
            t.join(timeout=10)
            assert result == ["canceled"]
            # Unlike pg_terminate_backend, the victim connection stays open.
            assert victim.execute("select 1").fetchone() == (1,)

    def test_unknown_pid_returns_false(self, dsn):
        with psycopg.connect(dsn, autocommit=True) as c:
            assert c.execute("select pg_cancel_backend(999999999)").fetchone() == (False,)


class TestActivityQueryText:
    def test_extended_protocol_query_keeps_placeholders(self, dsn):
        # The pgx liveness-poll shape: a parameterized pg_stat_activity query
        # must not match its own row (real PG shows the $1 placeholder).
        with psycopg.connect(dsn, autocommit=True) as c:
            marker = f"selfmatch {time.time()}"
            rows = c.execute(
                "select 1 from pg_stat_activity where query like %s",
                (f"%{marker}%",),
            ).fetchall()
            assert rows == []
