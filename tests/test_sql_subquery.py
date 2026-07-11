"""WHERE subqueries.

Non-correlated ``IN`` / ``NOT IN`` / scalar ``= (SELECT …)`` are pre-evaluated by
the planner (the inner SELECT runs through the engine, so it may itself filter /
aggregate). ``EXISTS`` and *correlated* subqueries (those that reference the
outer row) can't push down to a Mongo filter, so they're evaluated per row by
the scalar evaluator — the inner query reads inner-table rows with outer-row
references falling through to the enclosing row.
"""

from __future__ import annotations

import bson
import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
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
    try:
        yield s
    finally:
        s.close()


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


def test_exists_non_correlated_non_empty(storage, session):
    # A non-correlated EXISTS over a non-empty relation is true for every row.
    assert names(
        storage,
        session,
        "SELECT name FROM customers WHERE EXISTS (SELECT 1 FROM orders) ORDER BY name",
    ) == ["alice", "bob", "carol"]


def test_exists_non_correlated_empty_is_false(storage, session):
    assert (
        names(
            storage,
            session,
            "SELECT name FROM customers WHERE EXISTS (SELECT 1 FROM orders WHERE total > 1000)",
        )
        == []
    )


def test_correlated_exists(storage, session):
    # Customers that have at least one order (cust 1 and 3 do; 2 doesn't).
    assert names(
        storage,
        session,
        "SELECT name FROM customers c WHERE EXISTS "
        "(SELECT 1 FROM orders o WHERE o.cust = c._id) ORDER BY name",
    ) == ["alice", "carol"]


def test_correlated_not_exists(storage, session):
    assert names(
        storage,
        session,
        "SELECT name FROM customers c WHERE NOT EXISTS "
        "(SELECT 1 FROM orders o WHERE o.cust = c._id) ORDER BY name",
    ) == ["bob"]


def test_correlated_exists_with_outer_predicate(storage, session):
    # region='east' AND has-an-order: alice qualifies, bob has no order.
    assert names(
        storage,
        session,
        "SELECT name FROM customers c WHERE region = 'east' AND EXISTS "
        "(SELECT 1 FROM orders o WHERE o.cust = c._id) ORDER BY name",
    ) == ["alice"]


def test_correlated_in_subquery(storage, session):
    # c._id IN (custs whose order total exceeds c._id): alice(1) and carol(3).
    assert names(
        storage,
        session,
        "SELECT name FROM customers c WHERE c._id IN "
        "(SELECT o.cust FROM orders o WHERE o.total > c._id) ORDER BY name",
    ) == ["alice", "carol"]


def test_correlated_scalar_subquery(storage, session):
    # c._id = max(cust over orders whose total exceeds c._id) -> only carol (3=3).
    assert names(
        storage,
        session,
        "SELECT name FROM customers c WHERE c._id = "
        "(SELECT max(o.cust) FROM orders o WHERE o.total > c._id) ORDER BY name",
    ) == ["carol"]


def test_correlated_exists_count_star(storage, session):
    assert names(
        storage,
        session,
        "SELECT count(*) FROM customers c WHERE EXISTS "
        "(SELECT 1 FROM orders o WHERE o.cust = c._id)",
    ) == [2]


def test_correlated_exists_order_by_limit(storage, session):
    # ORDER BY / LIMIT apply to the per-row-filtered survivors, in order.
    assert names(
        storage,
        session,
        "SELECT name FROM customers c WHERE EXISTS "
        "(SELECT 1 FROM orders o WHERE o.cust = c._id) ORDER BY name DESC LIMIT 1",
    ) == ["carol"]


def test_multi_column_subquery_rejected(storage, session):
    with pytest.raises(SQLError) as ei:
        names(
            storage,
            session,
            "SELECT name FROM customers WHERE _id IN (SELECT cust, total FROM orders)",
        )
    assert ei.value.sqlstate == "0A000"


# -- scalar subquery in the SELECT list -------------------------------------- #


def rows_of(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0].rows


def test_scalar_subquery_in_select_uncorrelated(storage, session):
    # A subquery containing an aggregate in the SELECT list of a non-grouped query
    # must not make the outer query look grouped (previously raised 42803).
    got = rows_of(
        storage,
        session,
        "SELECT name, (SELECT max(total) FROM orders) AS m FROM customers ORDER BY name",
    )
    assert got == [("alice", 100), ("bob", 100), ("carol", 100)]


def test_scalar_subquery_in_select_correlated(storage, session):
    # A correlated subquery in the SELECT list is evaluated per row.
    got = rows_of(
        storage,
        session,
        "SELECT name, (SELECT count(*) FROM orders o WHERE o.cust = customers._id) AS n "
        "FROM customers ORDER BY name",
    )
    assert got == [("alice", 1), ("bob", 0), ("carol", 1)]


def test_scalar_subquery_in_grouped_select(storage, session):
    # A grouped query may also project a scalar subquery — it routes to the
    # evaluated group path.
    got = rows_of(
        storage,
        session,
        "SELECT region, count(*) AS c, (SELECT max(total) FROM orders) AS m "
        "FROM customers GROUP BY region ORDER BY region",
    )
    assert got == [("east", 2, 100), ("west", 1, 100)]


def test_scalar_subquery_does_not_mask_real_group_by_error(storage, session):
    # A genuinely ungrouped column is still rejected even with a subquery present.
    with pytest.raises(SQLError) as ei:
        rows_of(
            storage,
            session,
            "SELECT name, count(*) FROM customers",
        )
    assert ei.value.sqlstate == "42803"
