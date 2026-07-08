"""JOIN combined with GROUP BY / aggregates / HAVING.

The join builds a `$lookup` + `$unwind` chain; the GROUP BY then appends a
`$group` whose keys and accumulators resolve through the join resolver, so
`a.region` / `SUM(b.amt)` map to the post-unwind field paths. WHERE applies
before the group (post-join `$match`), HAVING after.
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
            {"_id": bson.Int64(11), "cust": bson.Int64(1), "total": bson.Int64(50)},
            {"_id": bson.Int64(12), "cust": bson.Int64(2), "total": bson.Int64(200)},
            {"_id": bson.Int64(13), "cust": bson.Int64(3), "total": bson.Int64(30)},
        ],
    )
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0].rows


def test_join_group_by_sum(storage, session):
    rows = q(
        storage,
        session,
        "SELECT c.region, SUM(o.total) AS s FROM orders o "
        "JOIN customers c ON o.cust = c._id GROUP BY c.region ORDER BY c.region",
    )
    assert rows == [("east", 350), ("west", 30)]


def test_join_group_by_count(storage, session):
    rows = q(
        storage,
        session,
        "SELECT c.name, COUNT(*) AS n FROM orders o "
        "JOIN customers c ON o.cust = c._id GROUP BY c.name ORDER BY c.name",
    )
    assert rows == [("alice", 2), ("bob", 1), ("carol", 1)]


def test_join_aggregate_without_group(storage, session):
    rows = q(
        storage,
        session,
        "SELECT SUM(o.total) AS s FROM orders o JOIN customers c ON o.cust = c._id",
    )
    assert rows == [(380,)]


def test_join_group_by_having(storage, session):
    rows = q(
        storage,
        session,
        "SELECT c.region, SUM(o.total) AS s FROM orders o "
        "JOIN customers c ON o.cust = c._id GROUP BY c.region "
        "HAVING SUM(o.total) > 200 ORDER BY c.region",
    )
    assert rows == [("east", 350)]


def test_join_group_by_with_where(storage, session):
    # WHERE filters joined rows before grouping.
    rows = q(
        storage,
        session,
        "SELECT c.region, COUNT(*) AS n FROM orders o "
        "JOIN customers c ON o.cust = c._id WHERE o.total > 40 GROUP BY c.region ORDER BY c.region",
    )
    assert rows == [("east", 3)]


def test_join_group_by_avg_min_max(storage, session):
    rows = q(
        storage,
        session,
        "SELECT c.region, AVG(o.total) AS a, MIN(o.total) AS mn, MAX(o.total) AS mx "
        "FROM orders o JOIN customers c ON o.cust = c._id GROUP BY c.region ORDER BY c.region",
    )
    assert rows == [("east", pytest.approx(116.6666667), 50, 200), ("west", 30.0, 30, 30)]


def test_join_group_array_agg(storage, session):
    rows = q(
        storage,
        session,
        "SELECT c.region, array_agg(o.total) AS totals FROM orders o "
        "JOIN customers c ON o.cust = c._id GROUP BY c.region ORDER BY c.region",
    )
    east = next(r for r in rows if r[0] == "east")
    assert sorted(east[1]) == [50, 100, 200]


def test_join_group_having_unaggregated_column_rejected(storage, session):
    # A non-aggregate SELECT column that isn't in GROUP BY is a grouping error.
    with pytest.raises(SQLError) as ei:
        q(
            storage,
            session,
            "SELECT c.region, o.total FROM orders o "
            "JOIN customers c ON o.cust = c._id GROUP BY c.region",
        )
    assert ei.value.sqlstate == "42803"
