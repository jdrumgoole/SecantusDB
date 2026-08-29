"""``statement_timeout`` enforcement: a query running past the configured
timeout is cancelled with 57014. The clock is a per-statement / per-message-batch
deadline on the session that ``pg_sleep`` (and other cancellation points) check.
"""

from __future__ import annotations

import tempfile
import time

import psycopg
import pytest

from secantus.sql import errors, run_sql
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


def test_timeout_seconds_parsing():
    s = Session(database=DB)
    s.settings["statement_timeout"] = "2s"
    assert s.statement_timeout_seconds() == 2.0
    s.settings["statement_timeout"] = "2000"  # bare number == ms
    assert s.statement_timeout_seconds() == 2.0
    s.settings["statement_timeout"] = "1min"
    assert s.statement_timeout_seconds() == 60.0
    s.settings["statement_timeout"] = "0"
    assert s.statement_timeout_seconds() == 0.0


def test_armed_deadline_cancels_pg_sleep(tmp_path):
    st = Storage(str(tmp_path))
    try:
        s = Session(database=DB)
        s.statement_deadline = time.monotonic() + 0.5
        t0 = time.monotonic()
        with pytest.raises(errors.SQLError) as e:
            run_sql(st, DB, "SELECT pg_sleep(5)", session=s)
        assert e.value.sqlstate == "57014"
        assert "statement timeout" in e.value.message
        assert time.monotonic() - t0 < 2.0  # cancelled early, not after 5s
    finally:
        st.close()


def test_no_deadline_runs_to_completion(tmp_path):
    st = Storage(str(tmp_path))
    try:
        s = Session(database=DB)  # no statement_timeout
        r = run_sql(st, DB, "SELECT pg_sleep(0.05)", session=s)[-1]
        assert r.rows == [(None,)]  # pg_sleep returns void/NULL
    finally:
        st.close()


@pytest.fixture
def server():
    srv = SecantusPGServer(storage_path=tempfile.mkdtemp(), port=0)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


def test_statement_timeout_over_the_wire(server):
    host, port = server.address
    dsn = f"host={host} port={port} dbname={DB} user=u"
    with psycopg.connect(dsn, autocommit=True) as conn:
        conn.execute("SET statement_timeout='1s'")
        t0 = time.monotonic()
        with pytest.raises(psycopg.errors.Error) as e:
            conn.execute("SELECT pg_sleep(5)")
        assert e.value.sqlstate == "57014"
        assert time.monotonic() - t0 < 3.0
        # A fresh statement is not affected by the previous one's elapsed time.
        conn.execute("SELECT pg_sleep(0.1)")
