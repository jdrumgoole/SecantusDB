"""What isolation level the SQL server actually provides, measured not assumed.

The engine provides exactly ONE isolation level — WiredTiger's snapshot
isolation, which is what PostgreSQL calls REPEATABLE READ — and reports back
whichever level the client asked for. That is right for one of the three levels
and wrong for the other two, in opposite directions:

    autocommit write-write      matches PG exactly (blocks, both writes land)
    explicit txn, READ COMMITTED   we serialization-fail where PG completes
    explicit txn, SERIALIZABLE     we permit write skew, which PG forbids

Every expectation here was measured against a live PostgreSQL 14 on 2026-08-29,
including the two that pin a DIVERGENCE. Those two are named
``test_known_divergence_*`` so nobody reads them as conformance — the project's
own history has tests that pinned a limitation while looking like they pinned a
behaviour, three of which were found and rewritten the same week.

The backlog entry these came from said "second writer gets 40001; PG blocks and
proceeds", which understated it (an unretried client LOSES a write) and
overstated it (autocommit, the common path, is already correct).
"""

from __future__ import annotations

import tempfile
import threading
import time

import pytest

import pg_oracle

psycopg = pytest.importorskip("psycopg")

from secantus.sql import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402


@pytest.fixture
def dsn():
    storage = Storage(tempfile.mkdtemp())
    server = SecantusPGServer(storage=storage, port=0)
    server.start()
    host, port = server.address
    try:
        yield f"host={host} port={port} dbname=testdb user=secantus"
    finally:
        server.stop()
        storage.close()


def _seed(dsn_, table):
    with psycopg.connect(dsn_, autocommit=True) as c:
        c.execute(f"create table {table} (id int primary key, n int)")
        c.execute(f"insert into {table} values (1, 100)")


def _racing_update(dsn_, table, *, isolation=None):
    """A holds an uncommitted update for ~0.6s; B updates the same row.

    Returns ``(what_b_did, final_value)``.
    """
    a = psycopg.connect(dsn_, autocommit=True)
    b = psycopg.connect(dsn_, autocommit=True)
    outcome = []
    begin = f"begin isolation level {isolation}" if isolation else None
    try:
        if begin:
            a.execute(begin)
        else:
            a.execute("begin")
        a.execute(f"update {table} set n = n + 1 where id = 1")

        def writer_b():
            try:
                if begin:
                    b.execute(begin)
                    b.execute(f"update {table} set n = n + 10 where id = 1")
                    b.execute("commit")
                else:
                    b.execute(f"update {table} set n = n + 10 where id = 1")
                outcome.append("committed")
            except Exception as exc:  # noqa: BLE001 — the sqlstate is the result
                outcome.append(getattr(exc, "sqlstate", None) or "error")

        t = threading.Thread(target=writer_b)
        t.start()
        time.sleep(0.6)
        a.execute("commit")
        t.join(timeout=15)
        with psycopg.connect(dsn_, autocommit=True) as c:
            final = c.execute(f"select n from {table} where id = 1").fetchone()[0]
        return outcome[0] if outcome else "no result", final
    finally:
        a.close()
        b.close()


def test_autocommit_write_write_blocks_and_keeps_both_writes(dsn):
    """The common path, and it already matches PostgreSQL: the second writer
    waits for the first to commit, then applies to the COMMITTED value.

    100 + 1 + 10 = 111. A lost update would show 110 (B never saw A's write) or
    101 (B failed silently)."""
    _seed(dsn, "wc")
    what, final = _racing_update(dsn, "wc")
    assert what == "committed"
    assert final == 111


def test_known_divergence_read_committed_txn_serialization_fails(dsn):
    """PostgreSQL at READ COMMITTED blocks the second writer and completes it
    (final 111). We provide snapshot isolation for every explicit transaction,
    so the second writer gets 40001 and its write is LOST unless the client
    retries (final 101).

    This pins the CURRENT behaviour, not the desired one. Closing it needs
    per-statement snapshots inside one transaction, which WiredTiger does not
    offer — a redesign, not a fix. See the backlog."""
    _seed(dsn, "wc_rc")
    what, final = _racing_update(dsn, "wc_rc", isolation="read committed")
    assert what == "40001"
    assert final == 101


def test_repeatable_read_txn_matches_postgres(dsn):
    """At REPEATABLE READ our behaviour is exactly PostgreSQL's — the level we
    actually implement."""
    _seed(dsn, "wc_rr")
    what, final = _racing_update(dsn, "wc_rr", isolation="repeatable read")
    assert what == "40001"
    assert final == 101


def test_known_divergence_serializable_permits_write_skew(dsn):
    """We accept SERIALIZABLE, report `serializable`, and provide snapshot
    isolation — which permits write skew. PostgreSQL aborts one transaction
    with 40001; both of ours commit, and the invariant a client used
    SERIALIZABLE to protect is silently violated.

    Pinned so the over-claim is visible and testable rather than folklore."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("create table ws (id int primary key, colour text)")
        c.execute("insert into ws values (1,'black'),(2,'white')")
    a = psycopg.connect(dsn, autocommit=True)
    b = psycopg.connect(dsn, autocommit=True)
    try:
        a.execute("begin isolation level serializable")
        b.execute("begin isolation level serializable")
        a.execute("select count(*) from ws where colour='black'").fetchone()
        b.execute("select count(*) from ws where colour='white'").fetchone()
        a.execute("update ws set colour='white' where colour='black'")
        b.execute("update ws set colour='black' where colour='white'")
        a.execute("commit")
        b.execute("commit")  # PostgreSQL raises 40001 here
        with psycopg.connect(dsn, autocommit=True) as c:
            colours = sorted(r[0] for r in c.execute("select colour from ws").fetchall())
        # The swap happened both ways: write skew.
        assert colours == ["black", "white"]
    finally:
        a.close()
        b.close()


@pytest.mark.parametrize("level", ["read committed", "repeatable read", "serializable"])
def test_begin_isolation_level_is_reported_back(dsn, level):
    """`BEGIN ISOLATION LEVEL x` is honoured and echoed, matching PostgreSQL —
    note this reports what was REQUESTED, while the engine always runs snapshot
    isolation. The two divergence tests above are the consequence."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(f"begin isolation level {level}")
        assert c.execute("show transaction_isolation").fetchone()[0] == level
        c.execute("rollback")


def _pg_oracle_dsn():
    """Delegates to `pg_oracle`, the one probe the six oracle suites share."""
    return pg_oracle.dsn()


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_autocommit_and_repeatable_read_match_real_postgres(dsn):
    """The two cases we claim to match, checked against the real server rather
    than against a hand-derived number."""
    oracle = _pg_oracle_dsn()
    with psycopg.connect(oracle, autocommit=True) as c:
        c.execute("drop table if exists wc_oracle")
        c.execute("create table wc_oracle (id int primary key, n int)")
        c.execute("insert into wc_oracle values (1, 100)")
    _seed(dsn, "wc_oracle")

    assert _racing_update(dsn, "wc_oracle") == _racing_update(oracle, "wc_oracle")

    with psycopg.connect(oracle, autocommit=True) as c:
        c.execute("update wc_oracle set n = 100 where id = 1")
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("update wc_oracle set n = 100 where id = 1")
    ours = _racing_update(dsn, "wc_oracle", isolation="repeatable read")
    theirs = _racing_update(oracle, "wc_oracle", isolation="repeatable read")
    assert ours == theirs
