"""P5 tests: JOIN, GROUP BY, and aggregate functions.

End-to-end through ``run_sql`` over the in-memory ``FakeStorage`` (whose query +
aggregation semantics are the real engines), so the SQL→pipeline translation is
exercised against the same `$group`/`$lookup` the production server uses.
"""

from __future__ import annotations

from decimal import Decimal

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage():
    return FakeStorage()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def _sales(storage, session):
    q(storage, session, "CREATE TABLE sales (id bigint primary key, region text, amount int)")
    rows = [(1, "east", 10), (2, "east", 20), (3, "west", 30), (4, "west", 5)]
    for i, r, a in rows:
        q(storage, session, f"INSERT INTO sales (id, region, amount) VALUES ({i}, '{r}', {a})")


# -- aggregates / GROUP BY --------------------------------------------------- #


def test_group_by_with_all_aggregates(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, COUNT(*) AS n, SUM(amount) AS s, AVG(amount) AS a, "
        "MIN(amount) AS lo, MAX(amount) AS hi FROM sales GROUP BY region ORDER BY region",
    )
    assert [c.name for c in res.columns] == ["region", "n", "s", "a", "lo", "hi"]
    assert res.rows == [
        ("east", 2, 30, 15.0, 10, 20),
        ("west", 2, 35, 17.5, 5, 30),
    ]


def test_whole_table_aggregate_no_group(storage, session):
    _sales(storage, session)
    res = q(storage, session, "SELECT COUNT(*), SUM(amount), AVG(amount) FROM sales")
    assert [c.name for c in res.columns] == ["count", "sum", "avg"]
    assert res.rows == [(4, 65, 16.25)]


def test_count_column_excludes_nulls(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, v int)")
    q(storage, session, "INSERT INTO t (id, v) VALUES (1, 5), (2, NULL), (3, 9)")
    res = q(storage, session, "SELECT COUNT(*) AS all_rows, COUNT(v) AS non_null FROM t")
    assert res.rows == [(3, 2)]


def test_where_then_group(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, SUM(amount) AS s FROM sales WHERE amount >= 10 "
        "GROUP BY region ORDER BY region",
    )
    assert res.rows == [("east", 30), ("west", 30)]


def test_having_filters_groups(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, SUM(amount) AS s FROM sales GROUP BY region HAVING SUM(amount) > 30",
    )
    assert res.rows == [("west", 35)]


def test_having_references_unselected_aggregate(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, COUNT(*) AS n FROM sales GROUP BY region "
        "HAVING SUM(amount) > 25 ORDER BY region",
    )
    assert res.rows == [("east", 2), ("west", 2)]


def test_group_order_by_aggregate_limit(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, SUM(amount) AS s FROM sales GROUP BY region ORDER BY s DESC LIMIT 1",
    )
    assert res.rows == [("west", 35)]


def test_non_grouped_column_is_rejected(storage, session):
    _sales(storage, session)
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT region, amount FROM sales GROUP BY region")
    assert ei.value.sqlstate == "42803"


def test_numeric_sum_returns_decimal(storage, session):
    q(storage, session, "CREATE TABLE p (id bigint primary key, price numeric)")
    q(storage, session, "INSERT INTO p (id, price) VALUES (1, 1.50), (2, 2.25)")
    res = q(storage, session, "SELECT SUM(price) AS total FROM p")
    assert res.rows == [(Decimal("3.75"),)]


# -- JOIN -------------------------------------------------------------------- #


def _orders(storage, session):
    q(storage, session, "CREATE TABLE customers (id bigint primary key, name text, region text)")
    q(storage, session, "CREATE TABLE orders (id bigint primary key, cust_id bigint, total int)")
    q(
        storage,
        session,
        "INSERT INTO customers (id, name, region) VALUES (1,'alice','e'),(2,'bob','w')",
    )
    q(
        storage,
        session,
        "INSERT INTO orders (id, cust_id, total) VALUES (10,1,100),(11,1,200),(12,2,50),(13,9,999)",
    )


def test_inner_join_drops_unmatched(storage, session):
    _orders(storage, session)
    res = q(
        storage,
        session,
        "SELECT o.id, o.total, c.name FROM orders o "
        "JOIN customers c ON o.cust_id = c.id ORDER BY o.id",
    )
    assert [c.name for c in res.columns] == ["id", "total", "name"]
    # order 13 (cust_id 9) has no matching customer.
    assert res.rows == [(10, 100, "alice"), (11, 200, "alice"), (12, 50, "bob")]


def test_left_join_keeps_unmatched_as_null(storage, session):
    _orders(storage, session)
    res = q(
        storage,
        session,
        "SELECT o.id, c.name FROM orders o LEFT JOIN customers c ON o.cust_id = c.id ORDER BY o.id",
    )
    assert res.rows == [(10, "alice"), (11, "alice"), (12, "bob"), (13, None)]


def test_join_with_where_on_joined_table(storage, session):
    _orders(storage, session)
    res = q(
        storage,
        session,
        "SELECT o.id, c.name FROM orders o JOIN customers c ON o.cust_id = c.id "
        "WHERE c.region = 'e' ORDER BY o.id",
    )
    assert res.rows == [(10, "alice"), (11, "alice")]


def test_join_ambiguous_column_rejected(storage, session):
    _orders(storage, session)
    with pytest.raises(SQLError) as ei:
        # ``id`` exists in both tables.
        q(storage, session, "SELECT id FROM orders o JOIN customers c ON o.cust_id = c.id")
    assert ei.value.sqlstate == "42702"


def test_join_unknown_alias_rejected(storage, session):
    _orders(storage, session)
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT x.id FROM orders o JOIN customers c ON o.cust_id = c.id")
    assert ei.value.sqlstate == "42P01"
