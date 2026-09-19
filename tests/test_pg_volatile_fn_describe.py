"""Describe and Execute agree on the types of volatile session functions.

Describe cannot run ``pg_sleep`` / ``pg_notify`` / ``nextval`` / the advisory
locks / the large-object creators — that would sleep, notify, draw a value or
take a lock — so it reports a type from ``engine._VOLATILE_FN_TAGS`` instead.
Execute types the same call through the planner. When the two disagreed, a
statement worked until the driver prepared it: psycopg prepares on the fifth
run, and the next Bind compared the described shape with the executed one and
failed with 0A000 "cached plan must not change result type". ``select 1,
pg_sleep(0.25)`` broke on its seventh run that way.

The expected oids are PostgreSQL's (``pg_proc.prorettype``).
"""

from __future__ import annotations

import pytest

psycopg = pytest.importorskip("psycopg")

from secantus.sql.pgserver import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402

VOID, TEXT, BOOL, INT4, INT8, OID = 2278, 25, 16, 23, 20, 26

CASES = [
    ("select pg_sleep(0)", VOID),
    ("select pg_notify('c', 'p')", VOID),
    ("select pg_advisory_lock(1)", VOID),
    ("select pg_advisory_lock_shared(2)", VOID),
    ("select pg_advisory_unlock(1)", BOOL),
    ("select pg_advisory_unlock_shared(2)", BOOL),
    ("select pg_try_advisory_lock(3)", BOOL),
    ("select pg_try_advisory_lock_shared(4)", BOOL),
    ("select pg_advisory_unlock_all()", VOID),
    ("select set_config('application_name', 'x', false)", TEXT),
    ("select nextval('s')", INT8),
    ("select lo_creat(-1)", OID),
    ("select lo_create(0)", OID),
]


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
        c.prepare_threshold = 0  # prepare from the first run
        try:
            c.execute("create sequence s")
            yield c
        finally:
            c.close()
    finally:
        srv.stop()
        st.close()


@pytest.mark.parametrize("sql,oid", CASES, ids=[c[0] for c in CASES])
def test_prepared_repeats_keep_their_type(conn, sql: str, oid: int) -> None:
    for _ in range(4):
        cur = conn.execute(sql, prepare=True)
        assert cur.description[0].type_code == oid
        cur.fetchall()


def test_pg_sleep_beside_a_constant_survives_auto_prepare(conn) -> None:
    """The shape that found it: the default threshold, repeated past it."""
    conn.prepare_threshold = 5
    for _ in range(10):
        assert conn.execute("select 1, pg_sleep(0)").fetchone() == (1, None)
