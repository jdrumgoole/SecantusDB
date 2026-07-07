"""Monitoring views (#137): pg_catalog.pg_stat_activity + pg_stat_database backed
by the wire server's live-session registry. The embedded builders are driven
here against a hand-built ActivityRegistry over the real ``Storage``; the live
active-state behaviour is covered by the pg8000 wire test.
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql import run_sql
from secantus.sql.session import ActivityRegistry, Session
from secantus.storage import Storage

DB = "app"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def _run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def test_stat_activity_reflects_registered_sessions(storage):
    reg = ActivityRegistry()
    start = dt.datetime(2026, 1, 1, tzinfo=dt.timezone.utc)
    a = Session(database=DB, user="alice", backend_pid=101)
    a.backend_start = start
    a.state = "idle"
    a.current_query = "SELECT 1"
    a.client_addr = "10.0.0.1"
    a.settings["application_name"] = "app_a"
    b = Session(database="other", user="bob", backend_pid=202)
    b.state = "active"
    b.current_query = "UPDATE t SET x = 1"
    for s in (a, b):
        s.activity_registry = reg
        reg.register(s)

    rows = _run(
        storage,
        a,
        "SELECT pid, datname, usename, application_name, client_addr, state, query, backend_type "
        "FROM pg_catalog.pg_stat_activity ORDER BY pid",
    ).rows
    assert rows == [
        (101, DB, "alice", "app_a", "10.0.0.1", "idle", "SELECT 1", "client backend"),
        (202, "other", "bob", "", None, "active", "UPDATE t SET x = 1", "client backend"),
    ]


def test_stat_activity_backend_start_is_timestamp(storage):
    reg = ActivityRegistry()
    start = dt.datetime(2026, 3, 4, 5, 6, 7, tzinfo=dt.timezone.utc)
    a = Session(database=DB, user="alice", backend_pid=1)
    a.backend_start = start
    a.activity_registry = reg
    reg.register(a)
    rows = _run(
        storage, a, "SELECT backend_start FROM pg_catalog.pg_stat_activity WHERE pid = 1"
    ).rows
    assert rows == [(start,)]


def test_stat_database_counts_backends_per_db(storage):
    reg = ActivityRegistry()
    sessions = [
        Session(database=DB, user="u1", backend_pid=1),
        Session(database=DB, user="u2", backend_pid=2),
        Session(database="other", user="u3", backend_pid=3),
    ]
    for s in sessions:
        s.activity_registry = reg
        reg.register(s)
    rows = _run(
        storage,
        sessions[0],
        "SELECT datname, numbackends FROM pg_catalog.pg_stat_database ORDER BY datname",
    ).rows
    assert rows == [(DB, 2), ("other", 1)]


def test_unregister_removes_from_activity(storage):
    reg = ActivityRegistry()
    a = Session(database=DB, user="a", backend_pid=1)
    b = Session(database=DB, user="b", backend_pid=2)
    for s in (a, b):
        s.activity_registry = reg
        reg.register(s)
    reg.unregister(b)
    rows = _run(storage, a, "SELECT pid FROM pg_catalog.pg_stat_activity ORDER BY pid").rows
    assert rows == [(1,)]


def test_stat_activity_without_registry_shows_only_this_session(storage):
    # The embedded run_sql API has no server registry — the view reflects just
    # the calling session.
    s = Session(database=DB, user="solo", backend_pid=7)
    rows = _run(storage, s, "SELECT pid, usename FROM pg_catalog.pg_stat_activity").rows
    assert rows == [(7, "solo")]
