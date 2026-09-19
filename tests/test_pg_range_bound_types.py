"""``lower()`` / ``upper()`` of a range have the type Postgres gives them.

Measured 2026-09-18 on the Python pgserver, every datetime range reported its
bounds as ``timestamp with time zone``, because the planner answered with the
range's STORAGE element (tsrange and daterange bounds coerce through
timestamptz). Postgres types them by the range:

    lower(tsrange)    timestamp without time zone   (was: with)
    lower(tstzrange)  timestamp with time zone      (::text lost its ``+00``)
    lower(daterange)  date                          (was: timestamptz, midnight)
"""

from __future__ import annotations

import datetime as dt

import pytest

import pg_oracle

psycopg = pytest.importorskip("psycopg")

from secantus.sql.pgserver import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402

_SETUP = [
    "set timezone = 'UTC'",
    "drop table if exists rb",
    "create table rb (ts tsrange, tz tstzrange, d daterange, i int4range, n numrange)",
    "insert into rb values ('[2024-05-01 10:00:00.5,2024-05-02)',"
    " '[2024-05-01 10:00:00+00,2024-05-02+00)', '[2024-05-01,2024-05-03)',"
    " '[1,5)', '[1.5,2.5)')",
]
_QUERIES = [
    f"select pg_typeof({f}({c}))::text, {f}({c})::text from rb"
    for c in ("ts", "tz", "d", "i", "n")
    for f in ("lower", "upper")
]


def _answers(c) -> list:
    for stmt in _SETUP:
        c.execute(stmt)
    return [c.execute(q).fetchone() for q in _QUERIES]


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        c = psycopg.connect(
            host=host, port=port, user="postgres", dbname="postgres", autocommit=True
        )
        try:
            yield c
        finally:
            c.close()
    finally:
        srv.stop()
        st.close()


def test_types_and_text(conn) -> None:
    got = dict(zip(_QUERIES, _answers(conn), strict=True))
    q = {
        (f, c): f"select pg_typeof({f}({c}))::text, {f}({c})::text from rb"
        for f, c in [("lower", "ts"), ("lower", "tz"), ("lower", "d"), ("upper", "d")]
    }
    assert got[q["lower", "ts"]] == ("timestamp without time zone", "2024-05-01 10:00:00.5")
    assert got[q["lower", "tz"]] == ("timestamp with time zone", "2024-05-01 10:00:00+00")
    assert got[q["lower", "d"]] == ("date", "2024-05-01")
    assert got[q["upper", "d"]] == ("date", "2024-05-03")


def test_client_values(conn) -> None:
    _answers(conn)
    ts, tz, d = conn.execute("select lower(ts), lower(tz), lower(d) from rb").fetchone()
    assert ts == dt.datetime(2024, 5, 1, 10, 0, 0, 500000)  # naive
    assert tz == dt.datetime(2024, 5, 1, 10, tzinfo=dt.timezone.utc)
    assert d == dt.date(2024, 5, 1)


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_matches_real_postgres(conn) -> None:
    pg = pg_oracle.connect()
    assert pg is not None
    try:
        theirs = _answers(pg)
        # Self-check: the reference must type a tsrange bound WITHOUT a zone, or
        # this comparison is not testing what it claims to.
        assert theirs[0][0] == "timestamp without time zone", theirs[0]
        pg.execute("drop table rb")
    finally:
        pg.close()
    ours = _answers(conn)
    for q, t, o in zip(_QUERIES, theirs, ours, strict=True):
        assert o == t, f"{q}\n  postgres: {t}\n  secantus: {o}"
