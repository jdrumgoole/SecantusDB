"""ORDER BY NULL placement (NULLS FIRST / NULLS LAST + Postgres defaults).

Postgres orders NULL as though it were the largest value: ASC puts NULLs last,
DESC puts them first, and an explicit ``NULLS FIRST`` / ``NULLS LAST`` overrides.
Mongo's sort treats NULL/missing as the smallest value, so the SQL layer can't
delegate NULL placement to storage — it sorts in Python (single-table /
correlated / evaluated / set-op) or adds a companion null-rank field to the
``$sort`` (join / GROUP BY pipeline). These tests pin all those paths.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE t (id bigint primary key, n int)")
    # n values 5, NULL, 3, NULL, 8
    for i, n in [(1, 5), (2, None), (3, 3), (4, None), (5, 8)]:
        s.q(
            f"INSERT INTO t (id) VALUES ({i})"
            if n is None
            else f"INSERT INTO t (id, n) VALUES ({i}, {n})"
        )
    try:
        yield s
    finally:
        s.close()


def col(storage, session, sql):
    return [r[0] for r in run_sql(storage, DB, sql, session=session)[0].rows]


# -- single-table path ------------------------------------------------------ #


def test_asc_default_nulls_last(storage, session):
    assert col(storage, session, "SELECT n FROM t ORDER BY n") == [3, 5, 8, None, None]


def test_desc_default_nulls_first(storage, session):
    assert col(storage, session, "SELECT n FROM t ORDER BY n DESC") == [None, None, 8, 5, 3]


def test_asc_nulls_first(storage, session):
    assert col(storage, session, "SELECT n FROM t ORDER BY n NULLS FIRST") == [None, None, 3, 5, 8]


def test_asc_nulls_last_explicit(storage, session):
    assert col(storage, session, "SELECT n FROM t ORDER BY n ASC NULLS LAST") == [
        3,
        5,
        8,
        None,
        None,
    ]


def test_desc_nulls_last(storage, session):
    assert col(storage, session, "SELECT n FROM t ORDER BY n DESC NULLS LAST") == [
        8,
        5,
        3,
        None,
        None,
    ]


def test_nulls_last_with_limit(storage, session):
    # OFFSET/LIMIT slice the Postgres-ordered rows, not Mongo's.
    assert col(storage, session, "SELECT n FROM t ORDER BY n LIMIT 2") == [3, 5]
    assert col(storage, session, "SELECT n FROM t ORDER BY n DESC LIMIT 2") == [None, None]


# -- pipeline (join) path --------------------------------------------------- #


@pytest.fixture
def joined(session, tmp_path):
    s = Storage(str(tmp_path))
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE c (id bigint primary key, region text)")
    s.q("CREATE TABLE o (id bigint primary key, cid int, amt int)")
    s.q("INSERT INTO c (id, region) VALUES (1, 'e'), (2, 'w')")
    # o11 references a missing customer -> region is NULL after the LEFT join.
    s.q("INSERT INTO o (id, cid, amt) VALUES (10, 1, 5), (11, 99, 7)")
    try:
        yield s
    finally:
        s.close()


def test_join_order_nulls_last_default(joined, session):
    rows = run_sql(
        joined,
        DB,
        "SELECT c.region, o.amt FROM o LEFT JOIN c ON o.cid = c.id ORDER BY c.region",
        session=session,
    )[0].rows
    assert rows == [("e", 5), (None, 7)]


def test_join_order_nulls_first(joined, session):
    rows = run_sql(
        joined,
        DB,
        "SELECT c.region, o.amt FROM o LEFT JOIN c ON o.cid = c.id ORDER BY c.region NULLS FIRST",
        session=session,
    )[0].rows
    assert rows == [(None, 7), ("e", 5)]


def test_evaluated_order_nulls_first(joined, session):
    # Scalar expr in ORDER BY -> evaluated path.
    rows = run_sql(
        joined,
        DB,
        "SELECT UPPER(c.region) AS r, o.amt FROM o LEFT JOIN c ON o.cid = c.id "
        "ORDER BY UPPER(c.region) NULLS FIRST",
        session=session,
    )[0].rows
    assert rows == [(None, 7), ("E", 5)]


# -- GROUP BY pipeline path ------------------------------------------------- #


def test_group_by_order_nulls(session, tmp_path):
    s = Storage(str(tmp_path))
    try:
        s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
        s.q("CREATE TABLE g (id bigint primary key, k text, v int)")
        s.q("INSERT INTO g (id, v) VALUES (1, 10)")  # k NULL
        s.q("INSERT INTO g (id, k, v) VALUES (2, 'x', 20), (3, 'x', 5)")
        default = run_sql(
            s, DB, "SELECT k, SUM(v) AS s FROM g GROUP BY k ORDER BY k", session=session
        )[0]
        assert default.rows == [("x", 25), (None, 10)]
        first = run_sql(
            s, DB, "SELECT k, SUM(v) AS s FROM g GROUP BY k ORDER BY k NULLS FIRST", session=session
        )[0]
        assert first.rows == [(None, 10), ("x", 25)]
    finally:
        s.close()


# -- set-operation path ----------------------------------------------------- #


def test_setop_order_nulls(session, tmp_path):
    s = Storage(str(tmp_path))
    try:
        s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
        s.q("CREATE TABLE g (id bigint primary key, v int)")
        s.q("INSERT INTO g (id, v) VALUES (1, 10), (2, 20)")
        assert col(s, session, "SELECT v FROM g UNION SELECT NULL ORDER BY v") == [10, 20, None]
        assert col(s, session, "SELECT v FROM g UNION SELECT NULL ORDER BY v NULLS FIRST") == [
            None,
            10,
            20,
        ]
        assert col(s, session, "SELECT v FROM g UNION SELECT NULL ORDER BY v DESC") == [
            None,
            20,
            10,
        ]
    finally:
        s.close()
