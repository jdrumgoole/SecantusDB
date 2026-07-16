"""End-to-end tests for the embedded SQL engine (P0 spike).

These drive ``secantus.sql.run_sql`` against the real WiredTiger-backed
``Storage`` (per the no-FakeStorage rule): the SQL-to-Mongo translation is
exercised end to end against the same engines and persistence layer production
uses, so type round-trips (Decimal128, datetime, ObjectId), transactions, and
cross-session visibility are all real.
"""

from __future__ import annotations

import datetime as _dt
from decimal import Decimal

import bson
import pytest

from secantus.sql import SQLError, run_sql
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def sql(storage, statement):
    """Run one statement and return its single result."""
    return run_sql(storage, DB, statement)[0]


def _make_users(storage):
    sql(storage, "CREATE TABLE users (id bigint primary key, name text, age int, active boolean)")
    sql(
        storage,
        "INSERT INTO users (id, name, age, active) VALUES "
        "(1, 'alice', 30, true), (2, 'bob', 17, false), (3, 'carol', 42, true)",
    )


# --------------------------------------------------------------------------- #


def test_create_insert_select_all(storage):
    _make_users(storage)
    res = sql(storage, "SELECT id, name, age, active FROM users ORDER BY id")
    assert res.command_tag == "SELECT 3"
    assert [c.name for c in res.columns] == ["id", "name", "age", "active"]
    assert res.rows == [
        (1, "alice", 30, True),
        (2, "bob", 17, False),
        (3, "carol", 42, True),
    ]


def test_select_star_expands_columns(storage):
    _make_users(storage)
    res = sql(storage, "SELECT * FROM users ORDER BY id LIMIT 1")
    assert [c.name for c in res.columns] == ["id", "name", "age", "active"]
    assert res.rows == [(1, "alice", 30, True)]


def test_pk_maps_to_id_field(storage):
    _make_users(storage)
    # The PK column is stored as the document _id.
    assert storage.find_matching(DB, "users", {"_id": bson.Int64(1)})[0]["name"] == "alice"


def test_where_comparisons_and_and(storage):
    _make_users(storage)
    res = sql(storage, "SELECT name FROM users WHERE age >= 18 AND active = true ORDER BY name")
    assert res.rows == [("alice",), ("carol",)]


def _names(storage, where):
    return {r[0] for r in sql(storage, f"SELECT name FROM users WHERE {where}").rows}


def test_where_or_not_in_between(storage):
    _make_users(storage)
    assert _names(storage, "age < 18 OR age > 40") == {"bob", "carol"}
    assert _names(storage, "id IN (1, 3)") == {"alice", "carol"}
    assert _names(storage, "age BETWEEN 18 AND 40") == {"alice"}
    assert _names(storage, "NOT active = true") == {"bob"}


def test_where_like(storage):
    _make_users(storage)
    assert {r[0] for r in sql(storage, "SELECT name FROM users WHERE name LIKE 'a%'").rows} == {
        "alice"
    }
    assert {r[0] for r in sql(storage, "SELECT name FROM users WHERE name LIKE '_ob'").rows} == {
        "bob"
    }


def test_order_desc_limit_offset(storage):
    _make_users(storage)
    res = sql(storage, "SELECT name FROM users ORDER BY age DESC LIMIT 2 OFFSET 1")
    assert res.rows == [("alice",), ("bob",)]


def test_count_star(storage):
    _make_users(storage)
    res = sql(storage, "SELECT COUNT(*) FROM users WHERE active = true")
    assert res.columns[0].name == "count"
    assert res.rows == [(2,)]


def test_is_null_and_not_null(storage):
    sql(storage, "CREATE TABLE t (id bigint primary key, note text)")
    sql(storage, "INSERT INTO t (id, note) VALUES (1, 'hi'), (2, NULL)")
    assert sql(storage, "SELECT id FROM t WHERE note IS NULL").rows == [(2,)]
    assert sql(storage, "SELECT id FROM t WHERE note IS NOT NULL").rows == [(1,)]


def test_update(storage):
    _make_users(storage)
    res = sql(storage, "UPDATE users SET age = 18, name = 'robert' WHERE id = 2")
    assert res.command_tag == "UPDATE 1"
    assert sql(storage, "SELECT name, age FROM users WHERE id = 2").rows == [("robert", 18)]


def test_delete(storage):
    _make_users(storage)
    res = sql(storage, "DELETE FROM users WHERE age < 18")
    assert res.command_tag == "DELETE 1"
    assert sql(storage, "SELECT COUNT(*) FROM users").rows == [(2,)]


def test_drop_table(storage):
    _make_users(storage)
    assert sql(storage, "DROP TABLE users").command_tag == "DROP TABLE"
    with pytest.raises(SQLError) as ei:
        sql(storage, "SELECT * FROM users")
    assert ei.value.sqlstate == "42P01"


def test_numeric_and_timestamp_coercion(storage):
    sql(storage, "CREATE TABLE m (id bigint primary key, price numeric, at timestamptz)")
    sql(storage, "INSERT INTO m (id, price, at) VALUES (1, 19.99, '2020-01-02T03:04:05Z')")
    # numeric stored as Decimal128, surfaced as Decimal; timestamptz as datetime.
    stored = storage.find_matching(DB, "m", {})[0]
    assert isinstance(stored["price"], bson.Decimal128)
    assert isinstance(stored["at"], _dt.datetime)
    row = sql(storage, "SELECT price, at FROM m").rows[0]
    assert row[0] == Decimal("19.99")
    # The embedded run_sql API returns a stored timestamptz as tz-aware UTC (#141),
    # matching the PG-correct instant.
    assert row[1] == _dt.datetime(2020, 1, 2, 3, 4, 5, tzinfo=_dt.timezone.utc)


def test_date_literal_comparison_coerced(storage):
    sql(storage, "CREATE TABLE ev (id bigint primary key, at timestamptz)")
    sql(
        storage,
        "INSERT INTO ev (id, at) VALUES (1, '2020-01-01T00:00:00Z'), (2, '2021-06-01T00:00:00Z')",
    )
    res = sql(storage, "SELECT id FROM ev WHERE at >= '2021-01-01T00:00:00Z'")
    assert res.rows == [(2,)]


def test_duplicate_pk_raises_unique_violation(storage):
    _make_users(storage)
    with pytest.raises(SQLError) as ei:
        sql(storage, "INSERT INTO users (id, name, age, active) VALUES (1, 'dup', 1, true)")
    assert ei.value.sqlstate == "23505"


def test_not_null_violation(storage):
    sql(storage, "CREATE TABLE nn (id bigint primary key, name text not null)")
    with pytest.raises(SQLError) as ei:
        sql(storage, "INSERT INTO nn (id) VALUES (1)")
    assert ei.value.sqlstate == "23502"


def test_undefined_column(storage):
    _make_users(storage)
    with pytest.raises(SQLError) as ei:
        sql(storage, "SELECT nope FROM users")
    assert ei.value.sqlstate == "42703"


def test_self_join_is_supported(storage):
    # JOINs landed in P5 (see tests/test_sql_aggregate.py); a self-join on the
    # primary key returns each row once.
    _make_users(storage)
    res = sql(storage, "SELECT u.name FROM users u JOIN users v ON u.id = v.id ORDER BY u.name")
    assert res.rows == [("alice",), ("bob",), ("carol",)]


def test_duplicate_table_and_if_not_exists(storage):
    sql(storage, "CREATE TABLE x (id bigint primary key)")
    with pytest.raises(SQLError) as ei:
        sql(storage, "CREATE TABLE x (id bigint primary key)")
    assert ei.value.sqlstate == "42P07"
    # IF NOT EXISTS is a no-op, not an error.
    assert sql(storage, "CREATE TABLE IF NOT EXISTS x (id bigint primary key)").command_tag == (
        "CREATE TABLE"
    )


def test_multi_statement_returns_one_result_each(storage):
    results = run_sql(
        storage,
        DB,
        "CREATE TABLE q (id bigint primary key, n int);"
        "INSERT INTO q (id, n) VALUES (1, 10);"
        "SELECT n FROM q;",
    )
    assert [r.command_tag for r in results] == ["CREATE TABLE", "INSERT 0 1", "SELECT 1"]
    assert results[-1].rows == [(10,)]


# -- CREATE / DROP INDEX + transaction characteristics ----------------------- #


def test_create_index_maps_to_storage(storage):
    _make_users(storage)
    assert sql(storage, "CREATE INDEX ix_age ON users (age)").command_tag == "CREATE INDEX"
    assert sql(storage, "CREATE UNIQUE INDEX ux_name ON users (name DESC)").command_tag == (
        "CREATE INDEX"
    )
    ixs = {ix["name"]: ix for ix in storage.list_indexes(DB, "users")}
    assert ixs["ix_age"]["key"] == {"age": 1}
    assert ixs["ux_name"]["key"] == {"name": -1}
    assert ixs["ux_name"].get("unique") is True


def test_create_index_pk_column_maps_to_id(storage):
    # The PK column maps to the stored `_id` field.
    _make_users(storage)
    sql(storage, "CREATE INDEX ix_id ON users (id)")
    (ix,) = [i for i in storage.list_indexes(DB, "users") if i["name"] == "ix_id"]
    assert ix["key"] == {"_id": 1}


def test_create_index_duplicate_and_if_not_exists(storage):
    _make_users(storage)
    sql(storage, "CREATE INDEX ix_age ON users (age)")
    with pytest.raises(SQLError) as ei:
        sql(storage, "CREATE INDEX ix_age ON users (age)")
    assert ei.value.sqlstate == "42P07"
    assert sql(storage, "CREATE INDEX IF NOT EXISTS ix_age ON users (age)").command_tag == (
        "CREATE INDEX"
    )


def test_drop_index(storage):
    _make_users(storage)
    sql(storage, "CREATE INDEX ix_age ON users (age)")
    assert sql(storage, "DROP INDEX ix_age").command_tag == "DROP INDEX"
    # Real Storage always retains the mandatory `_id_` index (real-Mongo
    # behaviour); the dropped `ix_age` is the one that must be gone.
    remaining = {ix["name"] for ix in storage.list_indexes(DB, "users")}
    assert "ix_age" not in remaining
    assert remaining == {"_id_"}
    with pytest.raises(SQLError) as ei:
        sql(storage, "DROP INDEX ix_age")
    assert ei.value.sqlstate == "42704"
    assert sql(storage, "DROP INDEX IF EXISTS ix_age").command_tag == "DROP INDEX"


def test_transaction_characteristics_accepted(storage):
    # Single-node: isolation / read-only characteristics are accepted no-ops.
    assert sql(storage, "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE").command_tag == "SET"
    assert sql(storage, "SET TRANSACTION READ ONLY").command_tag == "SET"
    assert (
        sql(
            storage, "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL READ COMMITTED"
        ).command_tag
        == "SET"
    )
    assert sql(storage, "BEGIN ISOLATION LEVEL READ COMMITTED").command_tag == "BEGIN"
