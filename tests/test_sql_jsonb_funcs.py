"""jsonb manipulation functions — ``jsonb_set`` / ``jsonb_insert`` /
``jsonb_strip_nulls`` / ``jsonb_pretty`` / ``jsonb_object_keys`` and the ``#-``
delete-at-path operator.

jsonb is stored as a native embedded document (dict / list), so these evaluate in
Python over the structure. The ``path`` argument is a Postgres ``text[]`` (``'{a,b}'``
or a list); the value argument is parsed as JSON (``'5'`` -> 5) the way an implicit
``::jsonb`` cast would.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, data jsonb)")
    run(storage, session, 'INSERT INTO t VALUES (1, \'{"a": 1, "b": {"c": 2}, "n": null}\')')
    return storage


def test_jsonb_set_existing_key(t, session):
    assert run(t, session, "SELECT jsonb_set(data, '{a}', '5') FROM t").rows == [
        ({"a": 5, "b": {"c": 2}, "n": None},)
    ]


def test_jsonb_set_nested(t, session):
    assert run(t, session, "SELECT jsonb_set(data, '{b,c}', '99') FROM t").rows == [
        ({"a": 1, "b": {"c": 99}, "n": None},)
    ]


def test_jsonb_set_creates_missing(t, session):
    assert run(t, session, "SELECT jsonb_set(data, '{x}', '7') FROM t").rows == [
        ({"a": 1, "b": {"c": 2}, "n": None, "x": 7},)
    ]


def test_jsonb_set_object_value(t, session):
    assert run(t, session, "SELECT jsonb_set(data, '{a}', '{\"k\":1}') FROM t").rows == [
        ({"a": {"k": 1}, "b": {"c": 2}, "n": None},)
    ]


def test_jsonb_insert_new_key(t, session):
    assert run(t, session, "SELECT jsonb_insert(data, '{y}', '8') FROM t").rows == [
        ({"a": 1, "b": {"c": 2}, "n": None, "y": 8},)
    ]


def test_jsonb_insert_existing_key_is_noop(t, session):
    assert run(t, session, "SELECT jsonb_insert(data, '{a}', '99') FROM t").rows == [
        ({"a": 1, "b": {"c": 2}, "n": None},)
    ]


def test_jsonb_strip_nulls(t, session):
    assert run(t, session, "SELECT jsonb_strip_nulls(data) FROM t").rows == [
        ({"a": 1, "b": {"c": 2}},)
    ]


def test_jsonb_delete_at_path_top(t, session):
    assert run(t, session, "SELECT data #- '{a}' FROM t").rows == [({"b": {"c": 2}, "n": None},)]


def test_jsonb_delete_at_path_nested(t, session):
    assert run(t, session, "SELECT data #- '{b,c}' FROM t").rows == [
        ({"a": 1, "b": {}, "n": None},)
    ]


def test_jsonb_object_keys(t, session):
    assert run(t, session, "SELECT jsonb_object_keys(data) FROM t ORDER BY 1").rows == [
        ("a",),
        ("b",),
        ("n",),
    ]


def test_jsonb_pretty(t, session):
    out = run(t, session, "SELECT jsonb_pretty(data) FROM t").rows[0][0]
    assert '"a": 1' in out and "\n" in out


def test_result_types(t, session):
    cols = run(
        t, session, "SELECT jsonb_set(data,'{a}','5') AS s, data #- '{a}' AS d FROM t"
    ).columns
    assert [c.type_tag for c in cols] == ["json", "json"]


def test_jsonb_functions_leave_source_unchanged(t, session):
    run(t, session, "SELECT jsonb_set(data, '{a}', '5') FROM t")
    # The stored row is untouched (the function returns a copy).
    assert run(t, session, "SELECT data FROM t").rows == [({"a": 1, "b": {"c": 2}, "n": None},)]
