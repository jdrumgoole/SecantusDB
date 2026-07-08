"""Scalar / IN WHERE-subqueries in the pipeline planning paths.

The single-table pushdown always threaded a ``SubqueryCtx``; the pipeline
planners (JOIN / GROUP BY / evaluated / DISTINCT) did not, so a WHERE subquery
there raised ``0A000``. ``plan_pipeline_select`` now publishes the context for
the duration of planning, so ``WHERE x OP (SELECT …)`` and ``x IN (SELECT …)``
work in every query shape.
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
    s.q("CREATE TABLE o (id bigint primary key, cid int, amt int)")
    s.q("CREATE TABLE c (id bigint primary key, region text)")
    s.q("CREATE TABLE lim (id bigint primary key, cap int)")
    for i, cid, amt in [(1, 1, 10), (2, 1, 50), (3, 2, 30)]:
        s.q(f"INSERT INTO o (id, cid, amt) VALUES ({i}, {cid}, {amt})")
    s.q("INSERT INTO c (id, region) VALUES (1, 'e'), (2, 'w')")
    s.q("INSERT INTO lim (id, cap) VALUES (1, 40)")
    try:
        yield s
    finally:
        s.close()


def rows(storage, session, sql):
    return sorted(
        run_sql(storage, DB, sql, session=session)[0].rows, key=lambda r: tuple(map(str, r))
    )


def test_join_where_scalar_subquery(storage, session):
    assert rows(
        storage,
        session,
        "SELECT o.id, c.region FROM o JOIN c ON o.cid = c.id WHERE o.amt > (SELECT cap FROM lim)",
    ) == [(2, "e")]


def test_group_by_where_scalar_subquery(storage, session):
    assert rows(
        storage,
        session,
        "SELECT c.region, SUM(o.amt) AS s FROM o JOIN c ON o.cid = c.id "
        "WHERE o.amt < (SELECT cap FROM lim) GROUP BY c.region",
    ) == [("e", 10), ("w", 30)]


def test_single_table_group_where_scalar_subquery(storage, session):
    assert rows(
        storage,
        session,
        "SELECT cid, COUNT(*) AS n FROM o WHERE amt < (SELECT cap FROM lim) GROUP BY cid",
    ) == [(1, 1), (2, 1)]


def test_evaluated_select_where_scalar_subquery(storage, session):
    # A scalar function in the SELECT list routes to the evaluated path.
    assert rows(
        storage,
        session,
        "SELECT upper(c.region) AS r FROM o JOIN c ON o.cid = c.id "
        "WHERE o.amt > (SELECT cap FROM lim)",
    ) == [("E",)]


def test_evaluated_select_where_in_subquery(storage, session):
    assert rows(
        storage,
        session,
        "SELECT upper(region) AS r FROM c WHERE id IN (SELECT cid FROM o WHERE amt > 40)",
    ) == [("E",)]


def test_recursive_cte_term_scalar_subquery(storage, session):
    # A scalar subquery in a recursive term's WHERE (references a sibling CTE).
    res = run_sql(
        storage,
        DB,
        "WITH RECURSIVE bound AS (SELECT 4 AS cap), "
        "nums(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM nums WHERE n < (SELECT cap FROM bound)) "
        "SELECT n FROM nums ORDER BY n",
        session=session,
    )[0]
    assert res.rows == [(1,), (2,), (3,), (4,)]
