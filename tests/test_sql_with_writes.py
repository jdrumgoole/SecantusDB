"""``WITH … <write>`` — a CTE prefix on INSERT / UPDATE / DELETE.

The CTEs materialize the same way as for a SELECT, then the write body runs
against the CTE-aware backend + catalog overlay: an ``INSERT … SELECT FROM cte``
reads the CTE as its source, and an ``UPDATE`` / ``DELETE`` whose WHERE has a
subquery over a CTE resolves it. Writes forward to real storage.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage():
    s = FakeStorage()
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE src (id bigint primary key, region text, amount int)")
    s.q("CREATE TABLE dst (id bigint primary key, region text, amount int)")
    s.q("CREATE TABLE t (id bigint primary key, n int)")
    for i, r, a in [(1, "e", 10), (2, "e", 20), (3, "w", 30)]:
        s.q(f"INSERT INTO src (id, region, amount) VALUES ({i}, '{r}', {a})")
    for i, n in [(1, 5), (2, 15), (3, 25)]:
        s.q(f"INSERT INTO t (id, n) VALUES ({i}, {n})")
    return s


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def test_with_insert_select(storage, session):
    res = q(
        storage,
        session,
        "WITH big AS (SELECT id, region, amount FROM src WHERE amount >= 20) "
        "INSERT INTO dst (id, region, amount) SELECT id, region, amount FROM big",
    )
    assert res.command_tag == "INSERT 0 2"
    assert q(storage, session, "SELECT id, region FROM dst ORDER BY id").rows == [
        (2, "e"),
        (3, "w"),
    ]


def test_with_update_where_subquery(storage, session):
    res = q(
        storage,
        session,
        "WITH hi AS (SELECT id FROM t WHERE n > 10) "
        "UPDATE t SET n = 0 WHERE id IN (SELECT id FROM hi)",
    )
    assert res.command_tag == "UPDATE 2"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 5), (2, 0), (3, 0)]


def test_with_delete_where_subquery(storage, session):
    res = q(
        storage,
        session,
        "WITH gone AS (SELECT id FROM src WHERE region = 'e') "
        "DELETE FROM src WHERE id IN (SELECT id FROM gone)",
    )
    assert res.command_tag == "DELETE 2"
    assert q(storage, session, "SELECT id FROM src ORDER BY id").rows == [(3,)]


def test_with_insert_returning_computed(storage, session):
    res = q(
        storage,
        session,
        "WITH one AS (SELECT 42 AS v) "
        "INSERT INTO t (id, n) SELECT 9, v FROM one RETURNING id, n * 2 AS dbl",
    )
    assert res.rows == [(9, 84)]


def test_with_multiple_ctes_on_insert(storage, session):
    res = q(
        storage,
        session,
        "WITH a AS (SELECT id, amount FROM src WHERE region = 'e'), "
        "b AS (SELECT id, amount FROM a WHERE amount > 15) "
        "INSERT INTO dst (id, region, amount) SELECT id, 'x', amount FROM b",
    )
    assert res.command_tag == "INSERT 0 1"
    assert q(storage, session, "SELECT id, region, amount FROM dst ORDER BY id").rows == [
        (2, "x", 20)
    ]
