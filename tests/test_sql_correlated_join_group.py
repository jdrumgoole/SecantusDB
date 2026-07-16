"""Correlated / EXISTS WHERE in a query that both JOINs and GROUP BYs.

Slice #45 wired a correlated WHERE into the single-table GROUP BY path and the
plain JOIN path, but a query combining *both* a JOIN and a GROUP BY silently
dropped the WHERE (the join builder skips the ``$match`` for a correlated WHERE,
and the join+group planner didn't re-apply it). Now the WHERE is filtered per
joined row *after* the join prefix and *before* the ``$group`` — so only the
surviving joined rows are grouped.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE orders (id bigint primary key, cid int, amt int)")
    s.q("CREATE TABLE customers (id bigint primary key, region text)")
    s.q("CREATE TABLE shipments (id bigint primary key, oid int)")
    for i, c, a in [(1, 1, 10), (2, 1, 20), (3, 2, 30), (4, 2, 5)]:
        s.q(f"INSERT INTO orders (id, cid, amt) VALUES ({i}, {c}, {a})")
    for i, r in [(1, "e"), (2, "w")]:
        s.q(f"INSERT INTO customers (id, region) VALUES ({i}, '{r}')")
    for i, o in [(1, 1), (2, 3)]:  # shipments for orders 1 and 3
        s.q(f"INSERT INTO shipments (id, oid) VALUES ({i}, {o})")
    try:
        yield s
    finally:
        s.close()


def test_join_group_correlated_exists(storage):
    # Only orders that have a shipment are grouped: order 1 (region e, 10) and
    # order 3 (region w, 30).
    res = storage.q(
        "SELECT c.region, SUM(o.amt) AS s, COUNT(*) AS n "
        "FROM orders o JOIN customers c ON o.cid = c.id "
        "WHERE EXISTS (SELECT 1 FROM shipments sh WHERE sh.oid = o.id) "
        "GROUP BY c.region ORDER BY c.region"
    )
    assert res.rows == [("e", 10, 1), ("w", 30, 1)]


def test_join_group_correlated_not_exists(storage):
    # Orders WITHOUT a shipment: order 2 (region e, 20) and order 4 (region w, 5).
    res = storage.q(
        "SELECT c.region, SUM(o.amt) AS s "
        "FROM orders o JOIN customers c ON o.cid = c.id "
        "WHERE NOT EXISTS (SELECT 1 FROM shipments sh WHERE sh.oid = o.id) "
        "GROUP BY c.region ORDER BY c.region"
    )
    assert res.rows == [("e", 20), ("w", 5)]


def test_join_group_correlated_scalar(storage):
    # Keep orders that beat the max amount of the same customer's other orders:
    # order 2 (20 > 10) region e, order 3 (30 > 5) region w.
    res = storage.q(
        "SELECT c.region, COUNT(*) AS n "
        "FROM orders o JOIN customers c ON o.cid = c.id "
        "WHERE o.amt > (SELECT max(amt) FROM orders o2 WHERE o2.cid = o.cid AND o2.id <> o.id) "
        "GROUP BY c.region ORDER BY c.region"
    )
    assert res.rows == [("e", 1), ("w", 1)]


def test_join_group_correlated_with_having(storage):
    # HAVING prunes the groups after the correlated WHERE filters + groups.
    res = storage.q(
        "SELECT c.region, SUM(o.amt) AS s "
        "FROM orders o JOIN customers c ON o.cid = c.id "
        "WHERE EXISTS (SELECT 1 FROM shipments sh WHERE sh.oid = o.id) "
        "GROUP BY c.region HAVING SUM(o.amt) > 20 ORDER BY c.region"
    )
    assert res.rows == [("w", 30)]  # region e (10) pruned by HAVING


def test_join_group_correlated_and_plain_predicate(storage):
    # A plain predicate AND a correlated EXISTS in the same WHERE.
    res = storage.q(
        "SELECT c.region, COUNT(*) AS n "
        "FROM orders o JOIN customers c ON o.cid = c.id "
        "WHERE o.amt >= 10 AND EXISTS (SELECT 1 FROM shipments sh WHERE sh.oid = o.id) "
        "GROUP BY c.region ORDER BY c.region"
    )
    # Orders with a shipment: 1 (10) and 3 (30); both clear amt >= 10.
    assert res.rows == [("e", 1), ("w", 1)]
