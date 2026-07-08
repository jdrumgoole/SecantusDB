"""Correlated / EXISTS subqueries in the WHERE of JOIN and GROUP BY queries.

Previously a correlated subquery only resolved in a single-table SELECT (the
``CorrelatedSelectPlan`` path). A JOIN or GROUP BY routes to the aggregation
pipeline, where the WHERE can't lower to a ``$match``; the planner now carries
such a WHERE for per-row evaluation:

* JOIN — the whole WHERE runs per joined row *after* the pipeline (the join
  resolver supplies the outer scope).
* single-table GROUP BY — the WHERE runs per base doc *before* the ``$group``,
  so only the survivors are grouped.
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
    s.q("CREATE TABLE customers (id bigint primary key, name text, region text)")
    s.q("CREATE TABLE shipments (id bigint primary key, oid int)")
    for i, c, a in [(1, 1, 10), (2, 1, 50), (3, 2, 30), (4, 3, 5)]:
        s.q(f"INSERT INTO orders (id, cid, amt) VALUES ({i}, {c}, {a})")
    for i, n, r in [(1, "ann", "e"), (2, "bob", "w"), (3, "cat", "e")]:
        s.q(f"INSERT INTO customers (id, name, region) VALUES ({i}, '{n}', '{r}')")
    for i, o in [(1, 1), (2, 1), (3, 3)]:
        s.q(f"INSERT INTO shipments (id, oid) VALUES ({i}, {o})")
    try:
        yield s
    finally:
        s.close()


def test_join_correlated_exists(storage):
    # Orders that have a shipment (correlated to the outer order id).
    res = storage.q(
        "SELECT o.id, c.name FROM orders o JOIN customers c ON o.cid = c.id "
        "WHERE EXISTS (SELECT 1 FROM shipments s WHERE s.oid = o.id) ORDER BY o.id"
    )
    assert res.rows == [(1, "ann"), (3, "bob")]


def test_join_correlated_not_exists(storage):
    res = storage.q(
        "SELECT o.id FROM orders o JOIN customers c ON o.cid = c.id "
        "WHERE NOT EXISTS (SELECT 1 FROM shipments s WHERE s.oid = o.id) ORDER BY o.id"
    )
    assert res.rows == [(2,), (4,)]


def test_join_correlated_scalar(storage):
    # Orders whose amount beats the max of the same customer's *other* orders.
    res = storage.q(
        "SELECT o.id FROM orders o JOIN customers c ON o.cid = c.id "
        "WHERE o.amt > (SELECT max(amt) FROM orders o2 WHERE o2.cid = o.cid AND o2.id <> o.id) "
        "ORDER BY o.id"
    )
    assert res.rows == [(2,)]


def test_join_correlated_and_plain_predicate(storage):
    # A correlated EXISTS AND a plain comparison in the same WHERE.
    res = storage.q(
        "SELECT o.id FROM orders o JOIN customers c ON o.cid = c.id "
        "WHERE o.amt >= 10 AND EXISTS (SELECT 1 FROM shipments s WHERE s.oid = o.id) "
        "ORDER BY o.id"
    )
    # Orders with a shipment: 1 (amt 10) and 3 (amt 30); both clear amt >= 10.
    assert res.rows == [(1,), (3,)]


def test_group_correlated_exists(storage):
    # WHERE filters customers to those with an order, *then* GROUP BY region.
    res = storage.q(
        "SELECT c.region, COUNT(*) AS n FROM customers c "
        "WHERE EXISTS (SELECT 1 FROM orders o WHERE o.cid = c.id) "
        "GROUP BY c.region ORDER BY c.region"
    )
    assert res.rows == [("e", 2), ("w", 1)]


def test_group_correlated_exists_filters_out(storage):
    # Remove customer 2's order so region 'w' drops out entirely.
    storage.q("DELETE FROM orders WHERE cid = 2")
    res = storage.q(
        "SELECT c.region, COUNT(*) AS n FROM customers c "
        "WHERE EXISTS (SELECT 1 FROM orders o WHERE o.cid = c.id) "
        "GROUP BY c.region ORDER BY c.region"
    )
    assert res.rows == [("e", 2)]


def test_group_correlated_scalar_in_where(storage):
    # GROUP BY with a correlated scalar comparison in WHERE: keep customers whose
    # id is below the max order id referencing them (i.e. that have any order).
    res = storage.q(
        "SELECT c.region, COUNT(*) AS n FROM customers c "
        "WHERE 0 < (SELECT COUNT(*) FROM orders o WHERE o.cid = c.id) "
        "GROUP BY c.region ORDER BY c.region"
    )
    assert res.rows == [("e", 2), ("w", 1)]
