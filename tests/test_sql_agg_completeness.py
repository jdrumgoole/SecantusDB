"""GROUP BY / aggregate completeness (#167):

* an expression that *wraps* an aggregate in the SELECT list (``sum(x) + 1``),
* the ``GROUPING()`` super-aggregate helper over ROLLUP / CUBE / plain GROUP BY,
* ``DISTINCT`` aggregate inside ``HAVING`` over a JOIN.

Driven through ``run_sql`` over the real WiredTiger-backed ``Storage``.
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


def _t(storage, session):
    run(storage, session, "CREATE TABLE t (id int primary key, g text, x int)")
    for i, (g, x) in enumerate([("a", 1), ("a", 3), ("b", 10), ("b", 20)]):
        run(storage, session, f"INSERT INTO t VALUES ({i}, '{g}', {x})")


# -- expression over an aggregate -------------------------------------------- #


def test_sum_plus_literal(storage, session):
    _t(storage, session)
    assert rows(storage, session, "SELECT g, sum(x) + 1 AS s FROM t GROUP BY g ORDER BY g") == [
        ("a", 5),
        ("b", 31),
    ]


def test_function_over_aggregate(storage, session):
    _t(storage, session)
    assert rows(
        storage, session, "SELECT g, round(avg(x), 1) AS a FROM t GROUP BY g ORDER BY g"
    ) == [
        ("a", 2.0),
        ("b", 15.0),
    ]


def test_arithmetic_of_two_aggregates(storage, session):
    _t(storage, session)
    # a: sum 4 - min 1 = 3 ; b: sum 30 - min 10 = 20
    assert rows(
        storage, session, "SELECT g, sum(x) - min(x) AS d FROM t GROUP BY g ORDER BY g"
    ) == [("a", 3), ("b", 20)]


def test_whole_table_expr_over_aggregate(storage, session):
    _t(storage, session)
    assert rows(storage, session, "SELECT sum(x) + 1 FROM t") == [(35,)]


def test_bare_aggregate_still_works(storage, session):
    _t(storage, session)
    assert rows(storage, session, "SELECT g, sum(x) FROM t GROUP BY g ORDER BY g") == [
        ("a", 4),
        ("b", 30),
    ]


# -- GROUPING() -------------------------------------------------------------- #


def _sales(storage, session):
    run(
        storage, session, "CREATE TABLE sales (id int primary key, region text, dept text, amt int)"
    )
    for i, (r, d, a) in enumerate([("e", "x", 10), ("e", "y", 20), ("w", "x", 30)]):
        run(storage, session, f"INSERT INTO sales VALUES ({i}, '{r}', '{d}', {a})")


def test_grouping_rollup(storage, session):
    _sales(storage, session)
    r = rows(
        storage,
        session,
        "SELECT region, GROUPING(region) AS g, sum(amt) AS s "
        "FROM sales GROUP BY ROLLUP(region) ORDER BY region NULLS LAST",
    )
    assert r == [("e", 0, 30), ("w", 0, 30), (None, 1, 60)]


def test_grouping_cube_bitmask(storage, session):
    _sales(storage, session)
    r = rows(
        storage,
        session,
        "SELECT region, dept, GROUPING(region, dept) AS grd, sum(amt) AS s "
        "FROM sales GROUP BY CUBE(region, dept) "
        "ORDER BY region NULLS LAST, dept NULLS LAST",
    )
    # bitmask: region bit (high) + dept bit (low), 1 = rolled up
    assert r == [
        ("e", "x", 0, 10),
        ("e", "y", 0, 20),
        ("e", None, 1, 30),
        ("w", "x", 0, 30),
        ("w", None, 1, 30),
        (None, "x", 2, 40),
        (None, "y", 2, 20),
        (None, None, 3, 60),
    ]


def test_grouping_plain_group_by_is_zero(storage, session):
    _sales(storage, session)
    r = rows(
        storage,
        session,
        "SELECT region, GROUPING(region) AS g FROM sales GROUP BY region ORDER BY region",
    )
    assert r == [("e", 0), ("w", 0)]


# -- DISTINCT aggregate in HAVING over a JOIN -------------------------------- #


def test_join_distinct_in_having(storage, session):
    run(storage, session, "CREATE TABLE ord (id int primary key, cust text, prod text)")
    run(storage, session, "CREATE TABLE cust (name text primary key, region text)")
    run(storage, session, "INSERT INTO cust VALUES ('a', 'e'), ('b', 'w')")
    for i, (c, p) in enumerate([("a", "p1"), ("a", "p1"), ("a", "p2"), ("b", "p3")]):
        run(storage, session, f"INSERT INTO ord VALUES ({i}, '{c}', '{p}')")
    # region e (cust a) has 2 distinct prods; region w (cust b) has 1 → only e.
    r = rows(
        storage,
        session,
        "SELECT c.region FROM ord o JOIN cust c ON o.cust = c.name "
        "GROUP BY c.region HAVING count(DISTINCT o.prod) > 1 ORDER BY c.region",
    )
    assert r == [("e",)]
