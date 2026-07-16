"""jsonb containment / existence operators and jsonb_* functions.

These ride the reflected (schema-on-read) path — documents written as `pymongo`
would, queried via SQL. The WHERE operators (`@>`, `?`, `?|`, `?&`) compile to
Mongo filters; the functions are evaluated per row.

Note on `->` inside a function argument: sqlglot reads a bare `f(a->'k')` arrow
as a lambda, so navigated function arguments must be parenthesised (`f((a->'k'))`)
or use the `#>` form (`f(a #> '{k}')`). Bare navigation in WHERE / projection is
unaffected.
"""

from __future__ import annotations

import bson
import pytest

from secantus.sql import run_sql
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
        "docs",
        [
            {"_id": bson.Int64(1), "data": {"a": 1, "b": 2, "tags": ["x", "y"]}},
            {"_id": bson.Int64(2), "data": {"a": 9, "c": 3, "tags": ["y", "z"]}},
            {"_id": bson.Int64(3), "data": {"a": 1}},
        ],
    )
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def ids(res):
    return [row[0] for row in res.rows]


# -- containment / existence operators (WHERE) ------------------------------- #


def test_contains_object(storage, session):
    res = q(storage, session, "SELECT _id FROM docs WHERE data @> '{\"a\":1}' ORDER BY _id")
    assert ids(res) == [1, 3]


def test_contains_multiple_keys(storage, session):
    res = q(storage, session, 'SELECT _id FROM docs WHERE data @> \'{"a":1,"b":2}\' ORDER BY _id')
    assert ids(res) == [1]


def test_contains_nested_array(storage, session):
    # @> with an array value → the field array contains every listed element.
    res = q(storage, session, 'SELECT _id FROM docs WHERE data @> \'{"tags":["y"]}\' ORDER BY _id')
    assert ids(res) == [1, 2]


def test_contains_empty_object_matches_all(storage, session):
    res = q(storage, session, "SELECT _id FROM docs WHERE data @> '{}' ORDER BY _id")
    assert ids(res) == [1, 2, 3]


def test_key_exists(storage, session):
    res = q(storage, session, "SELECT _id FROM docs WHERE data ? 'c' ORDER BY _id")
    assert ids(res) == [2]


def test_key_exists_array_element(storage, session):
    # `?` against an array value tests element membership.
    res = q(storage, session, "SELECT _id FROM docs WHERE data->'tags' ? 'z' ORDER BY _id")
    assert ids(res) == [2]


def test_any_key_exists(storage, session):
    res = q(storage, session, "SELECT _id FROM docs WHERE data ?| array['b','c'] ORDER BY _id")
    assert ids(res) == [1, 2]


def test_all_keys_exist(storage, session):
    res = q(storage, session, "SELECT _id FROM docs WHERE data ?& array['a','b'] ORDER BY _id")
    assert ids(res) == [1]


def test_contained_by_field_lhs(storage, session):
    # ``field <@ const`` — the stored doc is a subset of the constant. Runs as a
    # COLLSCAN + per-row residual (it can't lower to a Mongo filter).
    res = q(storage, session, "SELECT _id FROM docs WHERE data <@ '{\"a\":1}' ORDER BY _id")
    assert ids(res) == [3]  # only {"a":1} is a subset of {"a":1}


def test_contained_by_field_lhs_wider_constant(storage, session):
    res = q(
        storage,
        session,
        'SELECT _id FROM docs WHERE data <@ \'{"a":1,"b":2,"tags":["x","y"]}\' ORDER BY _id',
    )
    assert ids(res) == [1, 3]


def test_contains_const_lhs_field_rhs_residual(storage, session):
    # ``const @> field`` is the mirror of ``field <@ const`` and also runs residual.
    res = q(
        storage,
        session,
        'SELECT _id FROM docs WHERE \'{"a":1,"b":2,"tags":["x","y"]}\' @> data ORDER BY _id',
    )
    assert ids(res) == [1, 3]


def test_contained_by_scalar_object(storage, session):
    assert q(storage, session, 'SELECT \'{"a":1}\'::jsonb <@ \'{"a":1,"b":2}\'::jsonb').rows == [
        (True,)
    ]
    assert q(storage, session, 'SELECT \'{"a":1}\'::jsonb @> \'{"a":1,"b":2}\'::jsonb').rows == [
        (False,)
    ]


def test_contained_by_scalar_nested_and_array(storage, session):
    assert q(
        storage, session, 'SELECT \'{"a":{"b":1}}\'::jsonb <@ \'{"a":{"b":1,"c":2}}\'::jsonb'
    ).rows == [(True,)]
    assert q(storage, session, "SELECT '[1,2]'::jsonb <@ '[1,2,3]'::jsonb").rows == [(True,)]


def test_contains_combines_with_and(storage, session):
    res = q(
        storage,
        session,
        "SELECT _id FROM docs WHERE data @> '{\"a\":1}' AND data ? 'b' ORDER BY _id",
    )
    assert ids(res) == [1]


# -- scalar jsonb functions -------------------------------------------------- #


def test_jsonb_build_object(storage, session):
    res = q(storage, session, "SELECT jsonb_build_object('k', 5, 'n', 'v') AS o")
    assert res.rows == [({"k": 5, "n": "v"},)]
    assert res.columns[0].type_tag == "json"


def test_jsonb_build_array(storage, session):
    res = q(storage, session, "SELECT jsonb_build_array(1, 2, 3) AS a")
    assert res.rows == [([1, 2, 3],)]
    assert res.columns[0].type_tag == "json"


def test_jsonb_array_length(storage, session):
    res = q(
        storage, session, "SELECT jsonb_array_length(data #> '{tags}') AS n FROM docs WHERE _id = 1"
    )
    assert res.rows == [(2,)]
    assert res.columns[0].type_tag == "int4"


def test_jsonb_typeof(storage, session):
    res = q(
        storage,
        session,
        "SELECT jsonb_typeof(data) AS t1, jsonb_typeof((data->'tags')) AS t2 "
        "FROM docs WHERE _id = 1",
    )
    assert res.rows == [("object", "array")]


# -- set-returning jsonb functions ------------------------------------------- #


def test_jsonb_array_elements(storage, session):
    res = q(
        storage,
        session,
        "SELECT jsonb_array_elements((data->'tags')) AS e FROM docs WHERE _id = 1",
    )
    assert [r[0] for r in res.rows] == ["x", "y"]
    assert res.columns[0].type_tag == "json"


def test_jsonb_object_keys(storage, session):
    res = q(storage, session, "SELECT jsonb_object_keys(data) AS k FROM docs WHERE _id = 1")
    assert sorted(r[0] for r in res.rows) == ["a", "b", "tags"]
    assert res.columns[0].type_tag == "text"
