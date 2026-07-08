"""The ``RETURNING`` clause on INSERT / UPDATE / DELETE.

RETURNING projects the affected rows back as a result set (the same projection
vocabulary as a SELECT list: ``*``, columns, aliases, jsonb navigation). INSERT
returns the inserted rows, UPDATE the post-image of the updated rows, and DELETE
the deleted rows.
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
    res = run_sql(
        s,
        DB,
        "CREATE TABLE t (id bigint primary key, name text, n int)",
        session=Session(database=DB),
    )
    assert res[0].command_tag == "CREATE TABLE"
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def test_insert_returning_columns(storage, session):
    res = run(
        storage,
        session,
        "INSERT INTO t (id, name, n) VALUES (1, 'a', 10), (2, 'b', 20) RETURNING id, name",
    )
    assert [c.name for c in res.columns] == ["id", "name"]
    assert res.rows == [(1, "a"), (2, "b")]
    assert res.command_tag == "INSERT 0 2"
    assert res.rowcount == 2


def test_insert_returning_star(storage, session):
    res = run(storage, session, "INSERT INTO t (id, name, n) VALUES (7, 'g', 70) RETURNING *")
    assert [c.name for c in res.columns] == ["id", "name", "n"]
    assert res.rows == [(7, "g", 70)]


def test_insert_returning_alias(storage, session):
    res = run(
        storage, session, "INSERT INTO t (id, name, n) VALUES (3, 'c', 30) RETURNING id AS pk"
    )
    assert [c.name for c in res.columns] == ["pk"]
    assert res.rows == [(3,)]


def test_update_returning_post_image(storage, session):
    run(storage, session, "INSERT INTO t (id, name, n) VALUES (1, 'a', 10), (2, 'b', 20)")
    res = run(storage, session, "UPDATE t SET n = 99 WHERE id = 1 RETURNING id, n")
    # RETURNING reflects the NEW value, not the pre-update one.
    assert res.rows == [(1, 99)]
    assert res.command_tag == "UPDATE 1"


def test_update_returning_filter_on_changed_field(storage, session):
    # The classic trap: the WHERE matches on the column being changed, so a naive
    # "re-query with the same filter after the update" would return nothing.
    run(storage, session, "INSERT INTO t (id, name, n) VALUES (1, 'old', 10), (2, 'old', 20)")
    res = run(storage, session, "UPDATE t SET name = 'new' WHERE name = 'old' RETURNING id, name")
    assert sorted(res.rows) == [(1, "new"), (2, "new")]
    assert res.command_tag == "UPDATE 2"


def test_delete_returning(storage, session):
    run(
        storage,
        session,
        "INSERT INTO t (id, name, n) VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30)",
    )
    res = run(storage, session, "DELETE FROM t WHERE n > 15 RETURNING id, name, n")
    assert sorted(res.rows) == [(2, "b", 20), (3, "c", 30)]
    assert res.command_tag == "DELETE 2"
    # The rows are really gone.
    remaining = run(storage, session, "SELECT id FROM t ORDER BY id")
    assert remaining.rows == [(1,)]


def test_delete_returning_star(storage, session):
    run(storage, session, "INSERT INTO t (id, name, n) VALUES (1, 'a', 10)")
    res = run(storage, session, "DELETE FROM t WHERE id = 1 RETURNING *")
    assert [c.name for c in res.columns] == ["id", "name", "n"]
    assert res.rows == [(1, "a", 10)]


def test_no_returning_is_plain_command(storage, session):
    res = run(storage, session, "INSERT INTO t (id, name, n) VALUES (1, 'a', 10)")
    assert res.command_tag == "INSERT 0 1"
    assert res.columns == []
    assert res.rows == []


def test_insert_returning_on_reflected(session, tmp_path):
    # A reflected collection (no CREATE TABLE): INSERT ... RETURNING surfaces the
    # inserted row, projecting the Mongo field names.
    s = Storage(str(tmp_path))
    try:
        s.insert(DB, "people", [{"_id": bson.Int64(1), "name": "alice"}])
        res = run(
            s, session, "INSERT INTO people (_id, name) VALUES (2, 'bob') RETURNING _id, name"
        )
        assert [c.name for c in res.columns] == ["_id", "name"]
        assert res.rows == [(2, "bob")]
        # The row is really stored.
        stored = s.find_matching(DB, "people", {"name": "bob"})
        assert stored[0]["_id"] == 2
    finally:
        s.close()


def test_update_returning_on_reflected(session, tmp_path):
    s = Storage(str(tmp_path))
    try:
        s.insert(DB, "people", [{"_id": bson.Int64(1), "name": "x", "age": bson.Int64(40)}])
        res = run(s, session, "UPDATE people SET age = 41 WHERE name = 'x' RETURNING _id, age")
        assert res.rows == [(1, 41)]
    finally:
        s.close()


def test_returning_unknown_column_rejected(storage, session):
    with pytest.raises(SQLError) as ei:
        run(storage, session, "INSERT INTO t (id, name, n) VALUES (1, 'a', 10) RETURNING nope")
    assert ei.value.sqlstate == "42703"  # undefined_column
