"""pg_typeof() and ``::regtype`` — static type introspection.

``pg_typeof(x)`` resolves at plan time from the same inference that types
RowDescription; ``'name'::regtype`` normalizes a type spelling to the canonical
pretty form so the two compare equal the way psycopg's type tests expect
(``select pg_typeof(%s) = 'smallint'::regtype``).
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql, typemap
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


@pytest.fixture
def storage(tmp_path):
    st = Storage(str(tmp_path))
    try:
        yield st
    finally:
        st.close()


def q(storage, sql: str):
    return run_sql(storage, DB, sql, session=Session(database=DB))[0]


@pytest.mark.parametrize(
    ("expr", "want"),
    [
        ("1", "integer"),
        ("1::int2", "smallint"),
        ("1::int8", "bigint"),
        ("1.5", "numeric"),
        ("1.5::float4", "real"),
        ("1.5::float8", "double precision"),
        ("'x'", "unknown"),
        ("'x'::text", "text"),
        ("true", "boolean"),
        ("NULL", "unknown"),
        ("now()", "timestamp with time zone"),
        ("'{1,2}'::int4[]", "integer[]"),
        ("1 + 1", "integer"),
        ("case when true then 1 else 2 end", "integer"),
    ],
)
def test_pg_typeof_constants(storage, expr, want):
    res = q(storage, f"select pg_typeof({expr})")
    assert res.rows == [(want,)]
    assert res.columns[0].name == "pg_typeof"


def test_pg_typeof_columns(storage):
    q(storage, "CREATE TABLE pt (a int4, sm int2, f real, name text, arr int4[])")
    q(storage, "INSERT INTO pt VALUES (1, 2, 1.5, 'x', '{1,2}')")
    res = q(
        storage,
        "SELECT pg_typeof(a), pg_typeof(sm), pg_typeof(f), pg_typeof(name), pg_typeof(arr) "
        "FROM pt",
    )
    assert res.rows == [("integer", "smallint", "real", "text", "integer[]")]


def test_pg_typeof_regtype_comparison(storage):
    assert q(storage, "select pg_typeof(1) = 'integer'::regtype").rows == [(True,)]
    assert q(storage, "select pg_typeof(1::int2) = 'int2'::regtype").rows == [(True,)]
    assert q(storage, "select pg_typeof(1.5::real) = 'float4'::regtype").rows == [(True,)]
    assert q(storage, "select pg_typeof(1) = 'bigint'::regtype").rows == [(False,)]


def test_normalize_regtype_spellings():
    assert typemap.normalize_regtype("int4") == "integer"
    assert typemap.normalize_regtype("int2") == "smallint"
    assert typemap.normalize_regtype("float4") == "real"
    assert typemap.normalize_regtype("bool") == "boolean"
    assert typemap.normalize_regtype("varchar") == "text"
    assert typemap.normalize_regtype("timestamptz") == "timestamp with time zone"
    assert typemap.normalize_regtype("int4[]") == "integer[]"
    assert typemap.normalize_regtype("no_such_type") == "no_such_type"
