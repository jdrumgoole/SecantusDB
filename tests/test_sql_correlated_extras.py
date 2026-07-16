"""Correlated subqueries in pipeline paths + enum ORDER BY in a correlated SELECT
(#170).

Pins the behaviour that a correlated WHERE / EXISTS / scalar subquery works over a
GROUP BY or JOIN pipeline (not only a single-table SELECT), that a correlated
single-table SELECT sorts an enum column by its declared label order (not
lexically), and — since #171 — that a correlated subquery in HAVING is evaluated
per grouped row. Driven through ``run_sql`` over the real WiredTiger-backed
``Storage``.
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
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return run(storage, session, sql).rows


def _seed(storage, session):
    run(storage, session, "CREATE TABLE sales (id int primary key, region text, amt int)")
    run(storage, session, "CREATE TABLE thresh (region text primary key, minamt int)")
    run(storage, session, "CREATE TABLE cust (name text primary key, region text)")
    run(storage, session, "CREATE TABLE ord (id int primary key, cust text, amt int)")
    run(
        storage,
        session,
        "INSERT INTO sales VALUES (1, 'e', 10), (2, 'e', 30), (3, 'w', 5), (4, 'w', 40)",
    )
    run(storage, session, "INSERT INTO thresh VALUES ('e', 20), ('w', 3)")
    run(storage, session, "INSERT INTO cust VALUES ('a', 'e'), ('b', 'w')")
    run(storage, session, "INSERT INTO ord VALUES (1, 'a', 10), (2, 'a', 30), (3, 'b', 40)")


def test_correlated_scalar_where_in_group_by(storage, session):
    _seed(storage, session)
    # amt > per-region threshold, then group: e keeps {30}, w keeps {5,40}.
    assert rows(
        storage,
        session,
        "SELECT region, sum(amt) FROM sales s "
        "WHERE amt > (SELECT minamt FROM thresh t WHERE t.region = s.region) "
        "GROUP BY region ORDER BY region",
    ) == [("e", 30), ("w", 45)]


def test_correlated_exists_in_group_by(storage, session):
    _seed(storage, session)
    assert rows(
        storage,
        session,
        "SELECT region, count(*) FROM sales s "
        "WHERE EXISTS (SELECT 1 FROM thresh t WHERE t.region = s.region AND s.amt > t.minamt) "
        "GROUP BY region ORDER BY region",
    ) == [("e", 1), ("w", 2)]


def test_correlated_where_over_join(storage, session):
    _seed(storage, session)
    # o.amt > per-region threshold across the join; e keeps id2(30), w keeps id3(40).
    assert rows(
        storage,
        session,
        "SELECT c.region, sum(o.amt) FROM ord o JOIN cust c ON o.cust = c.name "
        "WHERE o.amt > (SELECT minamt FROM thresh t WHERE t.region = c.region) "
        "GROUP BY c.region ORDER BY c.region",
    ) == [("e", 30), ("w", 40)]


def test_correlated_scalar_in_select_over_join(storage, session):
    _seed(storage, session)
    assert rows(
        storage,
        session,
        "SELECT c.name, (SELECT minamt FROM thresh t WHERE t.region = c.region) AS m "
        "FROM ord o JOIN cust c ON o.cust = c.name ORDER BY c.name",
    ) == [("a", 20), ("a", 20), ("b", 3)]


def test_correlated_subquery_in_having(storage, session):
    # #171: a correlated subquery in HAVING is evaluated per grouped row (the group
    # key resolves through the residual scope). e: sum 40 > thresh 20 → kept;
    # w: sum 45 > thresh 3 → kept.
    _seed(storage, session)
    assert rows(
        storage,
        session,
        "SELECT region, sum(amt) AS s FROM sales s GROUP BY region "
        "HAVING sum(amt) > (SELECT minamt FROM thresh t WHERE t.region = s.region) ORDER BY region",
    ) == [("e", 40), ("w", 45)]


def test_correlated_subquery_in_having_filters(storage, session):
    # A group whose aggregate fails the per-group threshold is dropped.
    _seed(storage, session)
    run(storage, session, "UPDATE thresh SET minamt = 100 WHERE region = 'e'")
    assert rows(
        storage,
        session,
        "SELECT region, sum(amt) AS s FROM sales s GROUP BY region "
        "HAVING sum(amt) > (SELECT minamt FROM thresh t WHERE t.region = s.region) ORDER BY region",
    ) == [("w", 45)]


# -- correlated WHERE + GROUP BY + window function combined (#171) ------------ #


def test_correlated_where_group_window(storage, session):
    # WHERE amt > per-region threshold (e:20 keeps {30}; w:3 keeps {5,40}), grouped,
    # then a window rank() over the group sums (30 → 1, 45 → 2).
    _seed(storage, session)
    assert rows(
        storage,
        session,
        "SELECT region, sum(amt) AS s, rank() OVER (ORDER BY sum(amt)) AS rk FROM sales s "
        "WHERE amt > (SELECT minamt FROM thresh t WHERE t.region = s.region) "
        "GROUP BY region ORDER BY region",
    ) == [("e", 30, 1), ("w", 45, 2)]


def test_correlated_where_join_group_window(storage, session):
    # The full stack: correlated WHERE + JOIN + GROUP BY + window. e keeps o.amt 30,
    # w keeps o.amt 40; rank by group sum (30 → 1, 40 → 2).
    _seed(storage, session)
    assert rows(
        storage,
        session,
        "SELECT c.region, sum(o.amt) AS s, rank() OVER (ORDER BY sum(o.amt)) AS rk "
        "FROM ord o JOIN cust c ON o.cust = c.name "
        "WHERE o.amt > (SELECT minamt FROM thresh t WHERE t.region = c.region) "
        "GROUP BY c.region ORDER BY c.region",
    ) == [("e", 30, 1), ("w", 40, 2)]


# -- enum ORDER BY in a correlated single-table SELECT ----------------------- #


def test_enum_order_in_correlated_select(storage, session):
    run(storage, session, "CREATE TYPE prio AS ENUM ('low', 'medium', 'high', 'urgent')")
    run(storage, session, "CREATE TABLE task (id int primary key, p prio)")
    run(storage, session, "CREATE TABLE act (id int primary key)")
    run(storage, session, "INSERT INTO act VALUES (0), (1), (2)")
    for i, p in enumerate(["high", "low", "medium"]):
        run(storage, session, f"INSERT INTO task VALUES ({i}, '{p}')")
    # A correlated EXISTS keeps the per-row path; ORDER BY the enum must follow the
    # declared order (low < medium < high), not lexical (high, low, medium).
    r = rows(
        storage,
        session,
        "SELECT p FROM task t WHERE EXISTS (SELECT 1 FROM act a WHERE a.id = t.id) ORDER BY p",
    )
    assert [x[0] for x in r] == ["low", "medium", "high"]
