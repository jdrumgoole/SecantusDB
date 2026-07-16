"""pg_typeof() and ``::regtype`` — static type introspection.

``pg_typeof(x)`` resolves at plan time from the same inference that types
RowDescription; ``'name'::regtype`` normalizes a type spelling to the canonical
pretty form so the two compare equal the way psycopg's type tests expect
(``select pg_typeof(%s) = 'smallint'::regtype``).
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql, typemap
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
        "SELECT pg_typeof(a), pg_typeof(sm), pg_typeof(f), pg_typeof(name), pg_typeof(arr) FROM pt",
    )
    assert res.rows == [("integer", "smallint", "real", "text", "integer[]")]


def test_pg_typeof_regtype_comparison(storage):
    assert q(storage, "select pg_typeof(1) = 'integer'::regtype").rows == [(True,)]
    assert q(storage, "select pg_typeof(1::int2) = 'int2'::regtype").rows == [(True,)]
    assert q(storage, "select pg_typeof(1.5::real) = 'float4'::regtype").rows == [(True,)]
    assert q(storage, "select pg_typeof(1) = 'bigint'::regtype").rows == [(False,)]


def test_regtype_from_oid(storage):
    # ``21::regtype`` (or '21'::regtype) resolves the OID to the type and prints
    # the same pretty spelling pg_typeof does — psycopg's test_repr_wrapper runs
    # ``select pg_typeof(%s) = %s::regtype`` passing the OID as an integer.
    assert q(storage, "select 21::regtype").rows == [("smallint",)]
    assert q(storage, "select '21'::regtype").rows == [("smallint",)]
    assert q(storage, "select 23::regtype").rows == [("integer",)]
    assert q(storage, "select 701::regtype").rows == [("double precision",)]
    assert q(storage, "select 26::regtype").rows == [("oid",)]
    assert q(storage, "select 1005::regtype").rows == [("smallint[]",)]
    assert q(storage, "select 1028::regtype").rows == [("oid[]",)]
    assert q(storage, "select pg_typeof(1::int2) = 21::regtype").rows == [(True,)]
    assert q(storage, "select pg_typeof(1::int2) = 23::regtype").rows == [(False,)]


def test_regtype_from_unknown_oid(storage):
    with pytest.raises(errors.SQLError) as ei:
        q(storage, "select 99999::regtype")
    assert ei.value.sqlstate == "42704"
    assert str(ei.value) == "type with OID 99999 does not exist"


def test_pg_typeof_cast_to_oid(storage):
    # ``pg_typeof(x)::oid`` — regtype casts to oid as the type's OID, described
    # with the oid type (26) and keeping Postgres' output column name.
    res = q(storage, "select pg_typeof(1::int2)::oid")
    assert res.rows == [(21,)]
    assert res.columns[0].name == "pg_typeof"
    assert res.columns[0].pg_oid == 26
    assert q(storage, "select pg_typeof(1)::oid").rows == [(23,)]
    assert q(storage, "select pg_typeof('{1,2}'::int4[])::oid").rows == [(1007,)]


def test_pg_typeof_of_oid_value(storage):
    res = q(storage, "select pg_typeof(1::oid)")
    assert res.rows == [("oid",)]


def test_normalize_regtype_spellings():
    assert typemap.normalize_regtype("int4") == "integer"
    assert typemap.normalize_regtype("int2") == "smallint"
    assert typemap.normalize_regtype("float4") == "real"
    assert typemap.normalize_regtype("bool") == "boolean"
    assert typemap.normalize_regtype("varchar") == "text"
    assert typemap.normalize_regtype("timestamptz") == "timestamp with time zone"
    assert typemap.normalize_regtype("int4[]") == "integer[]"
    assert typemap.normalize_regtype("no_such_type") == "no_such_type"
