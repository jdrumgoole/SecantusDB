"""Non-correlated WHERE subqueries: IN / NOT IN and scalar `= (SELECT ...)`.

The inner SELECT is run through the engine and pre-evaluated to a list (`$in`)
or a single value, so it may itself filter / aggregate. Correlated subqueries
and EXISTS are deferred (faithful 0A000).
"""

from __future__ import annotations

import bson
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
    s = FakeStorage()
    s.insert(
        DB,
        "customers",
        [
            {"_id": bson.Int64(1), "name": "alice", "region": "east"},
            {"_id": bson.Int64(2), "name": "bob", "region": "east"},
            {"_id": bson.Int64(3), "name": "carol", "region": "west"},
        ],
    )
    s.insert(
        DB,
        "orders",
        [
            {"_id": bson.Int64(10), "cust": bson.Int64(1), "total": bson.Int64(100)},
            {"_id": bson.Int64(11), "cust": bson.Int64(3), "total": bson.Int64(30)},
        ],
    )
    return s


def names(storage, session, sql):
    return [r[0] for r in run_sql(storage, DB, sql, session=session)[0].rows]


def test_in_subquery(storage, session):
    assert names(
        storage,
        session,
        "SELECT name FROM customers WHERE _id IN (SELECT cust FROM orders) ORDER BY name",
    ) == ["alice", "carol"]


def test_in_subquery_with_inner_where(storage, session):
    assert names(
        storage,
        session,
        "SELECT name FROM customers WHERE _id IN "
        "(SELECT cust FROM orders WHERE total > 50) ORDER BY name",
    ) == ["alice"]


def test_not_in_subquery(storage, session):
    assert names(
        storage,
        session,
        "SELECT name FROM customers WHERE _id NOT IN (SELECT cust FROM orders) ORDER BY name",
    ) == ["bob"]


def test_scalar_subquery_eq_aggregate(storage, session):
    assert names(
        storage, session, "SELECT name FROM customers WHERE _id = (SELECT max(cust) FROM orders)"
    ) == ["carol"]


def test_scalar_subquery_comparison(storage, session):
    assert (
        names(
            storage,
            session,
            "SELECT name FROM customers WHERE _id < (SELECT min(cust) FROM orders) ORDER BY name",
        )
        == []
    )


def test_subquery_combines_with_outer_predicate(storage, session):
    assert names(
        storage,
        session,
        "SELECT name FROM customers WHERE region = 'east' AND _id IN "
        "(SELECT cust FROM orders) ORDER BY name",
    ) == ["alice"]


def test_exists_is_deferred(storage, session):
    with pytest.raises(SQLError) as ei:
        names(storage, session, "SELECT name FROM customers WHERE EXISTS (SELECT 1 FROM orders)")
    assert ei.value.sqlstate == "0A000"


def test_correlated_subquery_is_deferred(storage, session):
    with pytest.raises(SQLError) as ei:
        names(
            storage,
            session,
            "SELECT name FROM customers c WHERE _id IN "
            "(SELECT cust FROM orders o WHERE o.total = c._id)",
        )
    assert ei.value.sqlstate == "0A000"


def test_multi_column_subquery_rejected(storage, session):
    with pytest.raises(SQLError) as ei:
        names(
            storage,
            session,
            "SELECT name FROM customers WHERE _id IN (SELECT cust, total FROM orders)",
        )
    assert ei.value.sqlstate == "0A000"
