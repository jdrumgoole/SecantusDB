"""DISTINCT inside aggregates: COUNT/SUM/AVG(DISTINCT x), and MIN/MAX(DISTINCT).

DISTINCT count/sum/avg compile to a ``$addToSet`` in the ``$group`` plus a
post-group ``$addFields`` that reduces the set ($size / $reduce). MIN/MAX are
unaffected by DISTINCT, so they run the ordinary accumulator.
"""

from __future__ import annotations

from decimal import Decimal

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


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def _sales(storage, session):
    q(storage, session, "CREATE TABLE sales (id bigint primary key, region text, amount int)")
    # east amounts: 10, 20, 20 (dup) ; west amounts: 30, 30 (dup)
    rows = [(1, "east", 10), (2, "east", 20), (3, "east", 20), (4, "west", 30), (5, "west", 30)]
    for i, r, a in rows:
        q(storage, session, f"INSERT INTO sales (id, region, amount) VALUES ({i}, '{r}', {a})")


def test_count_distinct_no_group(storage, session):
    _sales(storage, session)
    # distinct amounts across all rows: {10, 20, 30} -> 3
    res = q(storage, session, "SELECT COUNT(DISTINCT amount) AS c FROM sales")
    assert res.rows == [(3,)]
    assert [c.name for c in res.columns] == ["c"]


def test_count_distinct_vs_count(storage, session):
    _sales(storage, session)
    res = q(
        storage, session, "SELECT COUNT(amount) AS all_n, COUNT(DISTINCT amount) AS d FROM sales"
    )
    assert res.rows == [(5, 3)]


def test_count_distinct_grouped(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, COUNT(DISTINCT amount) AS d FROM sales GROUP BY region ORDER BY region",
    )
    # east distinct {10,20} -> 2 ; west distinct {30} -> 1
    assert res.rows == [("east", 2), ("west", 1)]


def test_sum_distinct(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, SUM(DISTINCT amount) AS s FROM sales GROUP BY region ORDER BY region",
    )
    # east distinct {10,20} -> 30 ; west distinct {30} -> 30
    assert res.rows == [("east", 30), ("west", 30)]


def test_avg_distinct(storage, session):
    _sales(storage, session)
    res = q(storage, session, "SELECT AVG(DISTINCT amount) AS a FROM sales")
    # distinct {10, 20, 30} -> mean 20.0
    assert res.rows == [(20.0,)]


def test_min_max_distinct_equal_plain(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT MIN(DISTINCT amount) AS mn, MAX(DISTINCT amount) AS mx FROM sales",
    )
    assert res.rows == [(10, 30)]


def test_count_distinct_ignores_nulls(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, v int)")
    q(storage, session, "INSERT INTO t (id, v) VALUES (1, 5)")
    q(storage, session, "INSERT INTO t (id, v) VALUES (2, 5)")
    q(storage, session, "INSERT INTO t (id, v) VALUES (3, 7)")
    q(storage, session, "INSERT INTO t (id, v) VALUES (4, NULL)")
    # distinct non-null values: {5, 7} -> 2
    assert q(storage, session, "SELECT COUNT(DISTINCT v) AS c FROM t").rows == [(2,)]


def test_sum_distinct_numeric_type(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, p numeric)")
    for i, p in [(1, "1.50"), (2, "1.50"), (3, "2.25")]:
        q(storage, session, f"INSERT INTO t (id, p) VALUES ({i}, {p})")
    res = q(storage, session, "SELECT SUM(DISTINCT p) AS s FROM t")
    # distinct {1.50, 2.25} -> 3.75, carried as numeric (Decimal)
    assert res.rows[0][0] == Decimal("3.75")


def test_count_distinct_over_join(storage, session):
    # DISTINCT aggregate on the join+GROUP path: count distinct products per region.
    q(storage, session, "CREATE TABLE cust (id bigint primary key, region text)")
    q(storage, session, "CREATE TABLE ord (id bigint primary key, cust_id bigint, product text)")
    for i, r in [(1, "east"), (2, "east"), (3, "west")]:
        q(storage, session, f"INSERT INTO cust (id, region) VALUES ({i}, '{r}')")
    orders = [(1, 1, "a"), (2, 1, "b"), (3, 2, "a"), (4, 3, "c"), (5, 3, "c")]
    for i, c, p in orders:
        q(storage, session, f"INSERT INTO ord (id, cust_id, product) VALUES ({i}, {c}, '{p}')")
    res = q(
        storage,
        session,
        "SELECT c.region, COUNT(DISTINCT o.product) AS d "
        "FROM ord o JOIN cust c ON o.cust_id = c.id GROUP BY c.region ORDER BY c.region",
    )
    # east products {a, b} -> 2 ; west products {c} -> 1
    assert res.rows == [("east", 2), ("west", 1)]


def test_distinct_in_having(storage, session):
    # DISTINCT aggregate inside HAVING is supported single-table (#166): east has
    # distinct amounts {10, 20} = 2, west has {30} = 1 → only east qualifies.
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region FROM sales GROUP BY region "
        "HAVING COUNT(DISTINCT amount) > 1 ORDER BY region",
    )
    assert res.rows == [("east",)]
