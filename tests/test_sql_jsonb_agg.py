"""jsonb aggregates + builders (#104): jsonb_agg / json_agg, jsonb_object_agg /
json_object_agg (aggregates), and to_jsonb / to_json / row_to_json (scalar
builders).

The aggregates ride the same ``$push`` + ``$project`` path as array_agg (typed
``json``); jsonb_object_agg pushes ``{k, v}`` pairs folded into an object with
``$arrayToObject``. The builders are the identity — values already store as
native Python that renders as json on the wire.
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


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, g int, k text, v int)")
    for i, (g, k, v) in enumerate([(1, "a", 10), (1, "b", 20), (2, "c", 30)], start=1):
        run(storage, session, f"INSERT INTO t VALUES ({i}, {g}, '{k}', {v})")
    return storage


# -- jsonb_agg / json_agg ----------------------------------------------------- #


def test_jsonb_agg_whole_table(t, session):
    assert val(t, session, "SELECT jsonb_agg(v) AS a FROM t") == [10, 20, 30]


def test_json_agg_whole_table(t, session):
    assert val(t, session, "SELECT json_agg(v) AS a FROM t") == [10, 20, 30]


def test_jsonb_agg_grouped(t, session):
    rows = run(t, session, "SELECT g, jsonb_agg(v) AS a FROM t GROUP BY g ORDER BY g").rows
    assert [tuple(r) for r in rows] == [(1, [10, 20]), (2, [30])]


def test_jsonb_agg_typed_json(t, session):
    assert col(t, session, "SELECT jsonb_agg(v) AS a FROM t").type_tag == "json"


def test_jsonb_agg_order_by(t, session):
    assert val(t, session, "SELECT jsonb_agg(v ORDER BY v DESC) AS a FROM t") == [30, 20, 10]


def test_jsonb_agg_of_text(t, session):
    assert val(t, session, "SELECT jsonb_agg(k ORDER BY k) AS a FROM t") == ["a", "b", "c"]


# -- jsonb_object_agg / json_object_agg --------------------------------------- #


def test_jsonb_object_agg_whole_table(t, session):
    assert val(t, session, "SELECT jsonb_object_agg(k, v) AS o FROM t") == {
        "a": 10,
        "b": 20,
        "c": 30,
    }


def test_json_object_agg_whole_table(t, session):
    assert val(t, session, "SELECT json_object_agg(k, v) AS o FROM t") == {
        "a": 10,
        "b": 20,
        "c": 30,
    }


def test_jsonb_object_agg_grouped(t, session):
    rows = run(
        t, session, "SELECT g, jsonb_object_agg(k, v) AS o FROM t GROUP BY g ORDER BY g"
    ).rows
    assert [tuple(r) for r in rows] == [(1, {"a": 10, "b": 20}), (2, {"c": 30})]


def test_jsonb_object_agg_typed_json(t, session):
    assert col(t, session, "SELECT jsonb_object_agg(k, v) AS o FROM t").type_tag == "json"


def test_jsonb_object_agg_key_coerced_to_text(storage, session):
    # An integer key is stringified (Postgres object keys are text).
    run(storage, session, "CREATE TABLE p (id int PRIMARY KEY, n int, v int)")
    run(storage, session, "INSERT INTO p VALUES (1, 7, 100)")
    run(storage, session, "INSERT INTO p VALUES (2, 8, 200)")
    assert val(storage, session, "SELECT jsonb_object_agg(n, v) AS o FROM p") == {
        "7": 100,
        "8": 200,
    }


# -- to_jsonb / to_json / row_to_json ----------------------------------------- #


def test_to_jsonb_scalar(storage, session):
    assert val(storage, session, "SELECT to_jsonb(5) AS j") == 5


def test_to_json_text(storage, session):
    assert val(storage, session, "SELECT to_json('hello') AS j") == "hello"


def test_to_jsonb_typed_json(storage, session):
    assert col(storage, session, "SELECT to_jsonb(5) AS j").type_tag == "json"


def test_to_jsonb_of_column(t, session):
    assert val(t, session, "SELECT to_jsonb(v) AS j FROM t WHERE id = 1") == 10


def test_to_jsonb_composes_with_build_object(t, session):
    assert val(
        t, session, "SELECT jsonb_build_object('x', to_jsonb(v)) AS j FROM t WHERE id = 1"
    ) == {"x": 10}


def test_row_to_json_composite(storage, session):
    run(storage, session, "CREATE TYPE addr AS (street text, zip int)")
    run(storage, session, "CREATE TABLE c (id int PRIMARY KEY, a addr)")
    run(storage, session, "INSERT INTO c VALUES (1, ROW('Main', 90210))")
    assert val(storage, session, "SELECT row_to_json(a) AS j FROM c") == {
        "street": "Main",
        "zip": 90210,
    }


def test_to_jsonb_composite(storage, session):
    run(storage, session, "CREATE TYPE pt AS (x int, y int)")
    run(storage, session, "CREATE TABLE c (id int PRIMARY KEY, p pt)")
    run(storage, session, "INSERT INTO c VALUES (1, ROW(3, 4))")
    assert val(storage, session, "SELECT to_jsonb(p) AS j FROM c") == {"x": 3, "y": 4}
