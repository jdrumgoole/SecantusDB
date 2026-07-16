"""SQL/JSON path queries (#101): the ``secantus.sql.jsonpath`` evaluator plus the
SQL surface — ``jsonb_path_query`` / ``jsonb_path_query_array`` /
``jsonb_path_exists`` / ``jsonb_path_match`` and the ``@?`` / ``@@`` operators.
"""

from __future__ import annotations

import json

import pytest

from secantus.sql import jsonpath as jp
from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"

DOC = {"a": {"b": 5}, "items": [{"x": 1}, {"x": 2}, {"x": 3}], "tags": ["p", "q"]}


# -- the evaluator (unit) ----------------------------------------------------- #


@pytest.mark.parametrize(
    "path,expected",
    [
        ("$.a.b", [5]),
        ("$.items[0].x", [1]),
        ("$.items[-1].x", [3]),
        ("$.tags[*]", ["p", "q"]),
        ("$.items[*].x", [1, 2, 3]),
        ("$.a.*", [5]),
        ("$.nope", []),
        ("$.items[*] ? (@.x == 2)", [{"x": 2}]),
        ("$.items[*] ? (@.x > 1)", [{"x": 2}, {"x": 3}]),
        ("$.items[*].x ? (@ >= 2)", [2, 3]),
        ("$.items[*] ? (@.x == 1 || @.x == 3)", [{"x": 1}, {"x": 3}]),
    ],
)
def test_query(path, expected):
    assert jp.query(DOC, path) == expected


def test_exists():
    assert jp.exists(DOC, "$.a.b") is True
    assert jp.exists(DOC, "$.nope") is False


def test_match_predicate():
    assert jp.match({"x": 5}, "$.x == 5") is True
    assert jp.match({"x": 5}, "$.x == 6") is False
    assert jp.match({"a": {"b": 5}}, "$.a.b > 1") is True
    assert jp.match({"x": 5, "y": 10}, "$.x == 5 && $.y == 10") is True


def test_unsupported_raises():
    with pytest.raises(jp.JsonPathError):
        jp.query(DOC, "$.a.size()")


# -- the SQL surface ---------------------------------------------------------- #


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
    run(storage, session, f"INSERT INTO t VALUES (1, '{json.dumps(DOC)}')")
    return storage


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


def test_jsonb_path_query(t, session):
    assert val(t, session, "SELECT jsonb_path_query(data, '$.a.b') FROM t") == 5


def test_jsonb_path_query_array(t, session):
    assert val(t, session, "SELECT jsonb_path_query_array(data, '$.items[*].x') FROM t") == [
        1,
        2,
        3,
    ]


def test_jsonb_path_query_array_type(t, session):
    assert col(t, session, "SELECT jsonb_path_query_array(data, '$.tags[*]') FROM t").type_tag == (
        "json"
    )


def test_jsonb_path_exists(t, session):
    assert val(t, session, "SELECT jsonb_path_exists(data, '$.a.b') FROM t") is True
    assert val(t, session, "SELECT jsonb_path_exists(data, '$.nope') FROM t") is False


def test_jsonb_path_exists_type(t, session):
    assert col(t, session, "SELECT jsonb_path_exists(data, '$.a') FROM t").type_tag == "bool"


def test_at_question_operator(t, session):
    assert val(t, session, "SELECT data @? '$.items[*] ? (@.x == 2)' FROM t") is True
    assert val(t, session, "SELECT data @? '$.items[*] ? (@.x == 99)' FROM t") is False


def test_at_question_type(t, session):
    assert col(t, session, "SELECT data @? '$.a' FROM t").type_tag == "bool"


def test_at_at_operator(t, session):
    assert val(t, session, "SELECT data @@ '$.a.b == 5' FROM t") is True
    assert val(t, session, "SELECT data @@ '$.a.b == 6' FROM t") is False


def test_jsonb_path_match(t, session):
    assert val(t, session, "SELECT jsonb_path_match(data, '$.a.b > 10') FROM t") is False


def test_null_propagation(storage, session):
    run(storage, session, "CREATE TABLE n (id int PRIMARY KEY, data jsonb)")
    run(storage, session, "INSERT INTO n VALUES (1, NULL)")
    assert val(storage, session, "SELECT jsonb_path_query(data, '$.a') FROM n") is None
    assert val(storage, session, "SELECT jsonb_path_exists(data, '$.a') FROM n") is None
