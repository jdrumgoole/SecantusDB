"""``COPY <table> TO`` keeps a timestamp's microseconds.

A ``timestamp`` / ``timestamptz`` is stored as a BSON date (whole
milliseconds) plus a hidden companion holding the remainder
(`secantus.sql.subms`). SELECT merged it back; table-form COPY TO read the
stored date alone, so it exported ``…00.412`` for a stored ``…00.412661`` in
BOTH the text and binary formats, while ``COPY (SELECT …) TO`` was exact.
Found by psycopg's ``test_copy_table_across[row]``, which flapped on whether a
random draw's microseconds landed on a whole millisecond. COPY TO is how a
table is exported or backed up, so every export was silently truncated.
"""

from __future__ import annotations

import datetime as dt

import pytest

psycopg = pytest.importorskip("psycopg")

from secantus.sql.pgserver import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402

T = dt.datetime(2024, 5, 1, 10, 0, 0, 412661)
TZ = T.replace(tzinfo=dt.timezone.utc)


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
            c.execute("set timezone = 'UTC'")
            c.execute("create table src (id int primary key, ts timestamp, tz timestamptz)")
            c.execute("insert into src values (1, %s, %s), (2, null, null)", (T, TZ))
            yield c
        finally:
            c.close()
    finally:
        srv.stop()
        st.close()


def test_text_format(conn) -> None:
    with conn.cursor().copy("copy src (id, ts, tz) to stdout") as cp:
        rows = list(cp.rows())
    assert rows == [
        ("1", "2024-05-01 10:00:00.412661", "2024-05-01 10:00:00.412661+00"),
        ("2", None, None),
    ]


def test_binary_format(conn) -> None:
    with conn.cursor().copy("copy src (id, ts, tz) to stdout (format binary)") as cp:
        cp.set_types(["int4", "timestamp", "timestamptz"])
        rows = list(cp.rows())
    assert rows == [(1, T, TZ), (2, None, None)]


@pytest.mark.parametrize("fmt", ["", " (format binary)"])
def test_copy_a_table_across(conn, fmt: str) -> None:
    """psycopg's own shape: COPY TO out of one table, COPY FROM into another."""
    conn.execute("create table dst (id int primary key, ts timestamp, tz timestamptz)")
    # Two connections, as psycopg's test has it: one connection cannot run two
    # COPY operations at once.
    with (
        psycopg.connect(conn.info.dsn, autocommit=True) as conn2,
        conn.cursor().copy(f"copy src to stdout{fmt}") as out,
        conn2.cursor().copy(f"copy dst from stdin{fmt}") as into,
    ):
        for data in out:
            into.write(data)
    got = conn.execute("select id, ts, tz from dst order by id").fetchall()
    assert got == [(1, T, TZ), (2, None, None)]
