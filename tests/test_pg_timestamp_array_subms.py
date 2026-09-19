"""``timestamp[]`` / ``timestamptz[]`` elements keep their microseconds.

A timestamp is stored as a BSON date (whole milliseconds) plus a hidden
companion holding the 0-999 microsecond remainder (`secantus.sql.subms`). That
companion only existed for scalar columns, and every ELEMENT of a timestamp
array is a BSON date too -- so a stored ``[…826829]`` read back as
``[…826000]``, while the same array as an expression was exact. The companion
of an array column is now the parallel list of remainders. Found by psycopg's
random-data COPY tests (``test_copy_table_across`` / ``test_copy_from_leaks``),
which failed whenever a draw put microseconds inside an array.
"""

from __future__ import annotations

import datetime as dt

import pytest

psycopg = pytest.importorskip("psycopg")

from secantus.sql.pgserver import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402

UTC = dt.timezone.utc
A = dt.datetime(2024, 5, 1, 10, 0, 0, 826829)
B = dt.datetime(1999, 12, 31, 23, 59, 59, 999999)
WHOLE = dt.datetime(2024, 5, 1, 10, 0, 0, 826000)


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
            c.execute("create table t (id int primary key, a timestamp[], z timestamptz[])")
            yield c
        finally:
            c.close()
    finally:
        srv.stop()
        st.close()


def _row(conn, row_id: int = 1):
    return conn.execute("select a, z from t where id = %s", (row_id,)).fetchone()


def test_insert_parameter_with_nulls(conn) -> None:
    a = [A, None, WHOLE, B]
    z = [A.replace(tzinfo=UTC), None]
    conn.execute("insert into t values (1, %s, %s)", (a, z))
    assert _row(conn) == (a, z)


def test_insert_literal_two_dimensional(conn) -> None:
    conn.execute(
        'insert into t (id, a) values (1, \'{{"2024-05-01 10:00:00.826829",NULL},'
        '{"1999-12-31 23:59:59.999999","2024-05-01 10:00:00.826"}}\')'
    )
    assert _row(conn)[0] == [[A, None], [B, WHOLE]]


def test_update_sets_and_clears_the_remainders(conn) -> None:
    conn.execute("insert into t (id, a) values (1, %s)", ([A],))
    conn.execute("update t set a = %s where id = 1", ([B, A],))
    assert _row(conn)[0] == [B, A]
    # Whole milliseconds only: the old remainders must not survive.
    conn.execute("update t set a = %s where id = 1", ([WHOLE, WHOLE],))
    assert _row(conn)[0] == [WHOLE, WHOLE]


@pytest.mark.parametrize("fmt", ["", " (format binary)"])
def test_copy_in_and_out(conn, fmt: str) -> None:
    a = [A, None, B]
    z = [B.replace(tzinfo=UTC)]
    with conn.cursor().copy(f"copy t (id, a, z) from stdin{fmt}") as cp:
        if fmt:
            cp.set_types(["int4", "timestamp[]", "timestamptz[]"])
        cp.write_row((1, a, z))
    assert _row(conn) == (a, z)
    with conn.cursor().copy(f"copy t (id, a, z) to stdout{fmt}") as cp:
        if fmt:
            cp.set_types(["int4", "timestamp[]", "timestamptz[]"])
            assert list(cp.rows()) == [(1, a, z)]
        else:
            assert list(cp.rows()) == [
                (
                    "1",
                    '{"2024-05-01 10:00:00.826829",NULL,"1999-12-31 23:59:59.999999"}',
                    '{"1999-12-31 23:59:59.999999+00"}',
                )
            ]
