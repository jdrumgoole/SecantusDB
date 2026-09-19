"""Predicates, DISTINCT and GROUP BY see a timestamp's microseconds.

A timestamp is stored as a BSON date (whole milliseconds) plus a hidden
companion with the 0-999 microsecond remainder (`secantus.sql.subms`); for a
``timestamp[]`` the companion is a parallel list. Storage and plain reads kept
the microseconds, but three paths compared the TRUNCATED stored value
(measured 2026-09-19, all present before the array companion existed):

* a ``WHERE`` on a timestamp array was pushed down as a filter on the stored
  dates, so ``a = ARRAY['…826829']``, ``x = ANY(a)`` and ``a @> …`` matched
  nothing and ``a <> …`` matched the equal row too;
* the per-row WHERE scopes (the outer row of a correlated / EXISTS query and
  the inner rows of its subquery) read columns without the companion, so
  ``EXISTS (… WHERE s2.t = s.t …)`` never matched;
* DISTINCT / GROUP BY on a timestamp array grouped the truncated arrays,
  merging ones that differ only in microseconds.
"""

from __future__ import annotations

import datetime as dt

import pytest

psycopg = pytest.importorskip("psycopg")

from secantus.sql.pgserver import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402

# Same millisecond, different microseconds: only the remainder tells them apart.
A = dt.datetime(2024, 5, 1, 10, 0, 0, 826829)
B = dt.datetime(2024, 5, 1, 10, 0, 0, 826100)
LIT = "'2024-05-01 10:00:00.826829'::timestamp"


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
            c.execute("create table s (id int primary key, t timestamp, a timestamp[])")
            c.execute(
                "insert into s values (1, %s, %s), (2, %s, %s), (3, %s, %s)",
                (A, [A], B, [B], A, [A, B]),
            )
            yield c
        finally:
            c.close()
    finally:
        srv.stop()
        st.close()


def _ids(conn, where: str) -> list[int]:
    return [r[0] for r in conn.execute(f"select id from s where {where} order by id")]


@pytest.mark.parametrize(
    ("where", "want"),
    [
        (f"a = array[{LIT}]", [1]),
        (f"{LIT} = any(a)", [1, 3]),
        (f"a @> array[{LIT}]", [1, 3]),
        (f"a <> array[{LIT}]", [2, 3]),
    ],
)
def test_array_predicates(conn, where: str, want: list[int]) -> None:
    assert _ids(conn, where) == want


def test_correlated_exists_on_a_scalar_timestamp(conn) -> None:
    assert _ids(conn, "exists (select 1 from s s2 where s2.t = s.t and s2.id <> s.id)") == [1, 3]


def test_array_distinct_and_group_by(conn) -> None:
    assert conn.execute("select count(distinct a) from s").fetchone() == (3,)
    got = sorted(r[0] for r in conn.execute("select distinct a from s"))
    assert got == sorted([[A], [B], [A, B]])
    groups = dict((tuple(r[0]), r[1]) for r in conn.execute("select a, count(*) from s group by a"))
    assert groups == {(A,): 1, (B,): 1, (A, B): 1}
