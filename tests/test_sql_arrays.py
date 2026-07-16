"""Array columns — ``text[]`` / ``int[]`` declared columns, ``ARRAY[...]`` and
``'{...}'`` literals, ``= ANY(col)`` membership, ``@>`` containment, and the
``array_length`` / ``cardinality`` scalar functions.

Arrays are stored as native BSON lists (the Mongo-side representation), so
membership and containment reuse the existing array-aware query machinery. The
type tag is ``<elem>[]`` (``text[]``, ``int4[]``); the wire layer reports the
Postgres array type OID so a real driver decodes the ``{...}`` text into a list.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql, typemap
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


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, tags text[], nums int[])")
    run(storage, session, "INSERT INTO t VALUES (1, ARRAY['a','b'], ARRAY[1,2,3])")
    run(storage, session, "INSERT INTO t VALUES (2, '{x,y}', '{7,8}')")
    return storage


# -- typemap unit coverage ---------------------------------------------------- #


def test_type_tag_for_array():
    import sqlglot

    dt = sqlglot.parse_one("SELECT CAST(x AS text[])", dialect="postgres").find(
        sqlglot.exp.DataType
    )
    assert typemap.type_tag_for_sql(dt) == "text[]"


def test_is_array_tag_and_element():
    assert typemap.is_array_tag("int4[]")
    assert not typemap.is_array_tag("int4")
    assert typemap.array_element_tag("int4[]") == "int4"


def test_coerce_from_python_list():
    assert typemap.coerce([1, "2", 3], "int4[]") == [1, 2, 3]


def test_coerce_from_string_literal():
    assert typemap.coerce("{1,2,3}", "int4[]") == [1, 2, 3]


def test_parse_pg_array_literal_quoting_and_null():
    assert typemap._parse_pg_array_literal('{a,"b,c",NULL}') == ["a", "b,c", None]
    assert typemap._parse_pg_array_literal("{}") == []


def test_render_pg_array_quotes_special_elements():
    assert typemap.to_pg_text(["a", "b,c", None], "text[]") == b'{a,"b,c",NULL}'
    assert typemap.to_pg_text([1, 2, 3], "int4[]") == b"{1,2,3}"


def test_array_type_oid_registered():
    assert typemap.PG_OID["text[]"] == 1009
    assert typemap.PG_OID["int4[]"] == 1007


# -- store / round-trip ------------------------------------------------------- #


def test_array_literal_insert_and_select(t, session):
    r = run(t, session, "SELECT id, tags, nums FROM t ORDER BY id")
    assert r.rows == [(1, ["a", "b"], [1, 2, 3]), (2, ["x", "y"], [7, 8])]


def test_result_column_reports_array_oid(t, session):
    r = run(t, session, "SELECT tags, nums FROM t WHERE id = 1")
    oids = {c.name: c.pg_oid for c in r.columns}
    assert oids["tags"] == 1009
    assert oids["nums"] == 1007


# -- = ANY membership --------------------------------------------------------- #


def test_any_text_membership(t, session):
    assert run(t, session, "SELECT id FROM t WHERE 'a' = ANY(tags)").rows == [(1,)]


def test_any_int_membership(t, session):
    assert run(t, session, "SELECT id FROM t WHERE 7 = ANY(nums)").rows == [(2,)]


def test_any_no_match(t, session):
    assert run(t, session, "SELECT id FROM t WHERE 'zzz' = ANY(tags)").rows == []


def test_scalar_any_array_literal_is_in(t, session):
    # ``col = ANY(ARRAY[...])`` is Postgres' IN, not membership.
    assert run(t, session, "SELECT id FROM t WHERE id = ANY(ARRAY[2,3]) ORDER BY id").rows == [(2,)]


# -- @> containment ----------------------------------------------------------- #


def test_contains_array(t, session):
    assert run(t, session, "SELECT id FROM t WHERE nums @> ARRAY[7]").rows == [(2,)]


def test_contains_multiple(t, session):
    assert run(t, session, "SELECT id FROM t WHERE nums @> ARRAY[1,2]").rows == [(1,)]


# -- array_length / cardinality ---------------------------------------------- #


def test_array_length(t, session):
    r = run(t, session, "SELECT id, array_length(tags, 1) AS n FROM t ORDER BY id")
    assert r.rows == [(1, 2), (2, 2)]


def test_array_length_higher_dim_is_null(t, session):
    assert run(t, session, "SELECT array_length(nums, 2) FROM t WHERE id = 1").rows == [(None,)]


def test_cardinality(t, session):
    assert run(t, session, "SELECT cardinality(nums) FROM t WHERE id = 1").rows == [(3,)]


# -- reflection --------------------------------------------------------------- #


def test_information_schema_reports_array_data_type(t, session):
    r = run(
        t,
        session,
        "SELECT column_name, data_type FROM information_schema.columns "
        "WHERE table_name = 't' AND column_name IN ('tags','nums') ORDER BY column_name",
    )
    assert r.rows == [("nums", "ARRAY"), ("tags", "ARRAY")]


def test_pg_attribute_reports_array_oid(t, session):
    r = run(
        t,
        session,
        "SELECT attname, atttypid FROM pg_attribute "
        "WHERE attname IN ('tags','nums') ORDER BY attname",
    )
    assert r.rows == [("nums", 1007), ("tags", 1009)]
