"""P6 tests: reflected (schema-on-read) tables and jsonb navigation.

Documents are written straight into storage (as ``pymongo`` would) with no
``CREATE TABLE``, then queried via SQL — the dual-protocol read path.
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
    # Heterogeneous, nested, Mongo-written docs — no SQL DDL.
    s.insert(
        DB,
        "people",
        [
            {
                "_id": bson.Int64(1),
                "name": "alice",
                "age": bson.Int64(30),
                "profile": {"city": "NYC", "tags": ["a", "b"]},
            },
            {"_id": bson.Int64(2), "name": "bob", "age": bson.Int64(17), "profile": {"city": "LA"}},
            {"_id": bson.Int64(3), "name": "carol"},  # missing age / profile
        ],
    )
    return s


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


# --------------------------------------------------------------------------- #


def test_select_star_reflects_sampled_columns(storage, session):
    res = q(storage, session, "SELECT * FROM people ORDER BY _id")
    assert [c.name for c in res.columns] == ["_id", "name", "age", "profile"]
    assert res.rows[0] == (1, "alice", 30, {"city": "NYC", "tags": ["a", "b"]})
    # Missing fields read as NULL.
    assert res.rows[2] == (3, "carol", None, None)


def test_select_specific_columns(storage, session):
    res = q(storage, session, "SELECT name, age FROM people ORDER BY _id")
    assert res.rows == [("alice", 30), ("bob", 17), ("carol", None)]


def test_where_uses_inferred_numeric_type(storage, session):
    res = q(storage, session, "SELECT name FROM people WHERE age > 18")
    assert res.rows == [("alice",)]


def test_jsonb_extract_text(storage, session):
    res = q(storage, session, "SELECT name, profile->>'city' AS city FROM people ORDER BY _id")
    assert [c.name for c in res.columns] == ["name", "city"]
    assert res.rows == [("alice", "NYC"), ("bob", "LA"), ("carol", None)]


def test_jsonb_extract_in_where(storage, session):
    res = q(storage, session, "SELECT name FROM people WHERE profile->>'city' = 'LA'")
    assert res.rows == [("bob",)]


def test_jsonb_extract_object_returns_json(storage, session):
    # ``->`` (non-scalar) returns the nested value as jsonb.
    res = q(storage, session, "SELECT profile->'tags' AS tags FROM people WHERE _id = 1")
    assert res.rows == [(["a", "b"],)]
    assert res.columns[0].type_tag == "json"


def test_jsonb_hash_arrow_path(storage, session):
    res = q(storage, session, "SELECT profile #> '{city}' AS c FROM people WHERE _id = 2")
    assert res.rows == [("LA",)]


def test_nested_document_surfaces_as_json(storage, session):
    res = q(storage, session, "SELECT profile FROM people WHERE _id = 1")
    assert res.rows == [({"city": "NYC", "tags": ["a", "b"]},)]


def test_order_and_limit_over_reflected(storage, session):
    res = q(storage, session, "SELECT name FROM people ORDER BY name DESC LIMIT 2")
    assert res.rows == [("carol",), ("bob",)]


def test_unknown_collection_is_undefined_table(storage, session):
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT * FROM nonexistent")
    assert ei.value.sqlstate == "42P01"


def test_reflected_tables_are_read_only(storage, session):
    # No CREATE TABLE, so writes still report the relation as undefined.
    with pytest.raises(SQLError) as ei:
        q(storage, session, "INSERT INTO people (_id, name) VALUES (9, 'dave')")
    assert ei.value.sqlstate == "42P01"


def test_declared_table_takes_precedence(storage, session):
    # A declared table shadows reflection: its typed columns win.
    q(storage, session, "CREATE TABLE people2 (id bigint primary key, label text)")
    storage.insert(DB, "people2", [{"_id": bson.Int64(1), "label": "x", "extra": "hidden"}])
    res = q(storage, session, "SELECT * FROM people2")
    # Only declared columns appear; the un-declared `extra` field is not surfaced.
    assert [c.name for c in res.columns] == ["id", "label"]
    assert res.rows == [(1, "x")]


# -- aggregates / joins over reflected (schema-on-read) tables ---------------- #


@pytest.fixture
def sales_storage():
    s = FakeStorage()
    s.insert(
        DB,
        "sales",
        [
            {"_id": bson.Int64(1), "region": "east", "amount": bson.Int64(10)},
            {"_id": bson.Int64(2), "region": "east", "amount": bson.Int64(20)},
            {"_id": bson.Int64(3), "region": "west", "amount": bson.Int64(30)},
        ],
    )
    s.insert(
        DB,
        "customers",
        [{"_id": bson.Int64(1), "name": "alice"}, {"_id": bson.Int64(2), "name": "bob"}],
    )
    s.insert(
        DB,
        "orders",
        [
            {"_id": bson.Int64(10), "cust_id": bson.Int64(1), "total": bson.Int64(100)},
            {"_id": bson.Int64(11), "cust_id": bson.Int64(2), "total": bson.Int64(200)},
            {"_id": bson.Int64(12), "cust_id": bson.Int64(1), "total": bson.Int64(50)},
        ],
    )
    return s


def test_aggregate_over_reflected(sales_storage, session):
    res = q(sales_storage, session, "SELECT SUM(amount) AS total FROM sales")
    assert res.rows == [(60,)]


def test_group_by_over_reflected(sales_storage, session):
    res = q(
        sales_storage,
        session,
        "SELECT region, SUM(amount) AS s, COUNT(*) AS n FROM sales GROUP BY region ORDER BY region",
    )
    assert res.rows == [("east", 30, 2), ("west", 30, 1)]


def test_having_over_reflected(sales_storage, session):
    res = q(
        sales_storage,
        session,
        "SELECT region, SUM(amount) AS s FROM sales GROUP BY region HAVING SUM(amount) > 25",
    )
    assert res.rows == [("east", 30), ("west", 30)]


def test_join_over_reflected(sales_storage, session):
    # Reflected collections expose the Mongo field name `_id`, so the join keys
    # off `c._id` (there is no DDL declaring an `id` column).
    res = q(
        sales_storage,
        session,
        "SELECT c.name, o.total FROM orders o "
        "JOIN customers c ON o.cust_id = c._id ORDER BY c.name, o.total",
    )
    assert res.rows == [("alice", 50), ("alice", 100), ("bob", 200)]


def test_join_over_reflected_with_where(sales_storage, session):
    res = q(
        sales_storage,
        session,
        "SELECT o.total FROM orders o JOIN customers c ON o.cust_id = c._id "
        "WHERE c.name = 'alice' ORDER BY o.total",
    )
    assert res.rows == [(50,), (100,)]


def test_aggregate_over_unknown_collection(session):
    with pytest.raises(SQLError) as ei:
        q(FakeStorage(), session, "SELECT COUNT(*) AS n, region FROM ghost GROUP BY region")
    assert ei.value.sqlstate == "42P01"
