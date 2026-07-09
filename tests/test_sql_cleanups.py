"""Small correctness cleanups bundled together.

- FROM-less ``SELECT`` of a constant *expression* (arithmetic, ``||``, function
  calls) and a constant ``WHERE`` (false → zero rows).
- The jsonb ``<@`` (contained-by) operator: the pushable ``<constant> <@ field``
  form (equivalently ``field @> <constant>``) and, since #149, the residual
  ``field <@ <constant>`` form (a COLLSCAN + per-row containment check).
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
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


# -- FROM-less constant expressions ---------------------------------------- #


def test_fromless_arithmetic(storage, session):
    assert q(storage, session, "SELECT 1 + 1 AS two").rows == [(2,)]
    assert q(storage, session, "SELECT 2 * 3 AS six").rows == [(6,)]


def test_fromless_string_expr(storage, session):
    assert q(storage, session, "SELECT upper('a') || 'B' AS s").rows == [("AB",)]


def test_fromless_where_false_yields_no_rows(storage, session):
    res = q(storage, session, "SELECT 1 AS x WHERE 1 = 0")
    assert res.rows == []
    assert [c.name for c in res.columns] == ["x"]  # column shape preserved


def test_fromless_where_true_yields_row(storage, session):
    assert q(storage, session, "SELECT 1 AS x WHERE 1 = 1").rows == [(1,)]


def test_fromless_where_comparison(storage, session):
    assert q(storage, session, "SELECT 5 WHERE 2 > 3").rows == []
    assert q(storage, session, "SELECT 5 WHERE 3 > 2").rows == [(5,)]


def test_fromless_column_reference_is_undefined(storage, session):
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT foo")
    assert ei.value.sqlstate == "42703"


# -- jsonb <@ (contained by) ----------------------------------------------- #


@pytest.fixture
def docs(storage):
    storage.insert(
        DB,
        "docs",
        [
            {"_id": bson.Int64(1), "tags": ["py", "go"]},
            {"_id": bson.Int64(2), "tags": ["py"]},
            {"_id": bson.Int64(3), "tags": ["rust"]},
        ],
    )
    return storage


def test_jsonb_contained_by_constant_lhs(docs, session):
    # '["py"]' <@ tags  ==  tags @> '["py"]'  → rows whose tags contain "py".
    res = run_sql(
        docs, DB, "SELECT _id FROM docs WHERE '[\"py\"]'::jsonb <@ tags", session=session
    )[0]
    assert sorted(r[0] for r in res.rows) == [1, 2]


def test_jsonb_contained_by_field_lhs(docs, session):
    # ``tags <@ '[...]'`` (the stored value is a subset of the constant) can't lower
    # to a Mongo filter, so since #149 it runs as a COLLSCAN + per-row residual.
    res = run_sql(
        docs, DB, "SELECT _id FROM docs WHERE tags <@ '[\"py\"]'::jsonb", session=session
    )[0]
    assert sorted(r[0] for r in res.rows) == [2]  # only ["py"] is a subset of ["py"]
