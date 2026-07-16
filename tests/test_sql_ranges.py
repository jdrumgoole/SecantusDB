"""Range types (#103): int4range / int8range / numrange / tsrange / daterange.

A range value is stored as a subdocument ``{"lower", "upper", "lower_inc",
"upper_inc"}`` (or ``{"empty": True}``); discrete ranges canonicalise to the
``[)`` form. Constructors (``int4range(1,10)``), text literals (``'[1,10)'``),
the accessors ``lower`` / ``upper`` / ``isempty``, and the ``@>`` / ``<@`` / ``&&``
operators (in the SELECT list and in WHERE) are wired end to end.
"""

from __future__ import annotations

import datetime as _dt

import bson
import pytest

from secantus.sql import ranges, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure ranges.py module
# --------------------------------------------------------------------------- #


def test_make_range_basic():
    r = ranges.make_range(1, 10, "[)", "int4range")
    assert r == {"lower": 1, "upper": 10, "lower_inc": True, "upper_inc": False}


def test_make_range_discrete_canonicalises():
    # (1,10] -> [2,11) ; [1,10] -> [1,11)
    assert ranges.make_range(1, 10, "(]", "int4range") == {
        "lower": 2,
        "upper": 11,
        "lower_inc": True,
        "upper_inc": False,
    }
    assert ranges.make_range(1, 10, "[]", "int4range") == {
        "lower": 1,
        "upper": 11,
        "lower_inc": True,
        "upper_inc": False,
    }


def test_make_range_numrange_not_canonicalised():
    # Continuous ranges keep their bound flags verbatim; bounds store in the
    # subtype's canonical form (numeric -> Decimal128) whatever the input type.
    r = ranges.make_range(1.5, 3.5, "(]", "numrange")
    assert r == {
        "lower": bson.Decimal128("1.5"),
        "upper": bson.Decimal128("3.5"),
        "lower_inc": False,
        "upper_inc": True,
    }


def test_make_range_empty_collapses():
    assert ranges.make_range(5, 5, "[)", "int4range") == {"empty": True}
    assert ranges.make_range(10, 1, "[)", "int4range") == {"empty": True}


def test_make_range_singleton_inclusive_not_empty():
    assert ranges.is_empty(ranges.make_range(5, 5, "[]", "numrange")) is False


def test_daterange_step_is_one_day():
    d = _dt.datetime(2020, 1, 1)
    r = ranges.make_range(d, d, "[]", "daterange")
    assert r["upper"] == d + _dt.timedelta(days=1)
    assert r["upper_inc"] is False


def test_contains_value():
    r = ranges.make_range(1, 10, "[)", "int4range")
    assert ranges.contains_value(r, 1) is True
    assert ranges.contains_value(r, 9) is True
    assert ranges.contains_value(r, 10) is False  # upper exclusive
    assert ranges.contains_value(r, 0) is False
    assert ranges.contains_value(r, None) is False


def test_contains_value_unbounded():
    lo = ranges.make_range(None, 10, "()", "numrange")
    assert ranges.contains_value(lo, -1e9) is True
    assert ranges.contains_value(lo, 10) is False
    hi = ranges.make_range(0, None, "[)", "numrange")
    assert ranges.contains_value(hi, 1e9) is True


def test_contains_range():
    a = ranges.make_range(1, 20, "[)", "int4range")
    b = ranges.make_range(5, 10, "[)", "int4range")
    assert ranges.contains_range(a, b) is True
    assert ranges.contains_range(b, a) is False
    # Every range contains the empty range.
    assert ranges.contains_range(a, {"empty": True}) is True
    # The empty range contains nothing non-empty.
    assert ranges.contains_range({"empty": True}, b) is False


def test_overlaps():
    a = ranges.make_range(1, 10, "[)", "int4range")
    b = ranges.make_range(5, 20, "[)", "int4range")
    c = ranges.make_range(10, 20, "[)", "int4range")
    assert ranges.overlaps(a, b) is True
    assert ranges.overlaps(a, c) is False  # touch at 10 but a excludes 10
    assert ranges.overlaps(a, {"empty": True}) is False


def test_render():
    assert ranges.render(ranges.make_range(1, 10, "[)", "int4range")) == "[1,10)"
    assert ranges.render({"empty": True}) == "empty"
    assert ranges.render(ranges.make_range(None, 10, "()", "numrange")) == "(,10)"


def test_parse_literal_roundtrip():
    r = ranges.parse_literal("[1,10)", "int4range", int)
    assert r == ranges.make_range(1, 10, "[)", "int4range")
    assert ranges.parse_literal("empty", "int4range", int) == {"empty": True}
    # A (1,10] literal canonicalises just like the constructor.
    assert ranges.parse_literal("(1,10]", "int4range", int) == ranges.make_range(
        1, 10, "(]", "int4range"
    )


def test_parse_literal_unbounded():
    r = ranges.parse_literal("[1,)", "int4range", int)
    assert r["lower"] == 1 and r["upper"] is None


# --------------------------------------------------------------------------- #
# SQL surface
# --------------------------------------------------------------------------- #


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
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, r int4range)")
    run(storage, session, "INSERT INTO t VALUES (1, int4range(1,10))")
    run(storage, session, "INSERT INTO t VALUES (2, int4range(5,20))")
    run(storage, session, "INSERT INTO t VALUES (3, int4range(100,200))")
    return storage


def test_stored_as_subdocument(t, session):
    # The stored value round-trips as a normalised range subdoc.
    assert val(t, session, "SELECT r FROM t WHERE id = 1") == {
        "lower": 1,
        "upper": 10,
        "lower_inc": True,
        "upper_inc": False,
    }


def test_constructor_typed_as_range(t, session):
    assert col(t, session, "SELECT int4range(1,5)").type_tag == "int4range"


def test_text_literal_insert(storage, session):
    run(storage, session, "CREATE TABLE lit (id int PRIMARY KEY, r int4range)")
    run(storage, session, "INSERT INTO lit VALUES (1, '[3,7)')")
    assert val(storage, session, "SELECT r FROM lit WHERE id = 1") == ranges.make_range(
        3, 7, "[)", "int4range"
    )


def test_contains_value_operator(t, session):
    assert val(t, session, "SELECT r @> 7 FROM t WHERE id = 1") is True
    assert val(t, session, "SELECT r @> 50 FROM t WHERE id = 1") is False


def test_contains_operator_typed_bool(t, session):
    assert col(t, session, "SELECT r @> 7 FROM t").type_tag == "bool"


def test_accessors(t, session):
    row = run(t, session, "SELECT lower(r), upper(r), isempty(r) FROM t WHERE id = 1").rows[0]
    assert list(row) == [1, 10, False]


def test_accessor_element_type(t, session):
    result = run(t, session, "SELECT lower(r), upper(r), isempty(r) FROM t WHERE id = 1")
    assert result.columns[0].type_tag == "int4"
    assert result.columns[1].type_tag == "int4"
    assert result.columns[2].type_tag == "bool"


def test_cast_text_to_range(storage, session):
    r = val(storage, session, "SELECT '[1,10)'::int4range")
    assert r == ranges.make_range(1, 10, "[)", "int4range")


def test_where_contains_value(t, session):
    # 7 falls inside both [1,10) and [5,20).
    ids = [row[0] for row in run(t, session, "SELECT id FROM t WHERE r @> 7 ORDER BY id").rows]
    assert ids == [1, 2]


def test_where_overlaps(t, session):
    ids = [
        row[0]
        for row in run(t, session, "SELECT id FROM t WHERE r && int4range(15,150) ORDER BY id").rows
    ]
    assert ids == [2, 3]


def test_where_contained_by(t, session):
    ids = [
        row[0]
        for row in run(t, session, "SELECT id FROM t WHERE int4range(6,8) <@ r ORDER BY id").rows
    ]
    # [6,8) sits inside both [1,10) and [5,20).
    assert ids == [1, 2]


def test_where_contains_range(t, session):
    ids = [
        row[0]
        for row in run(t, session, "SELECT id FROM t WHERE r @> int4range(6,8) ORDER BY id").rows
    ]
    assert ids == [1, 2]


def test_numrange_contains(storage, session):
    assert val(storage, session, "SELECT numrange(1.5, 3.5) @> 2.0") is True
    assert val(storage, session, "SELECT numrange(1.5, 3.5) @> 4.0") is False


def test_empty_range_literal(storage, session):
    assert val(storage, session, "SELECT isempty('empty'::int4range)") is True


def test_pg_type_reports_range(storage, session):
    rows = run(
        storage,
        session,
        "SELECT typtype FROM pg_catalog.pg_type WHERE typname = 'int4range'",
    ).rows
    assert rows and rows[0][0] == "r"


# -- representation-independent equality + constructor bound canonicalisation -- #


def test_range_equality_across_construction_paths(storage, session):
    # A constructor's bounds and a text cast's bounds store differently
    # (int / Decimal vs Decimal128, date objects vs ISO text) — equality
    # compares the canonical identity.
    r = run(
        storage,
        session,
        "SELECT numrange(-100::numeric, 100.123::numeric, '(]') = '(-100,100.123]'::numrange",
    )
    assert r.rows == [(True,)]
    r = run(
        storage,
        session,
        "SELECT daterange('2000-01-01'::date, '2020-01-01'::date, '[)')"
        " = '[2000-01-01,2020-01-01)'::daterange",
    )
    assert r.rows == [(True,)]


def test_untyped_literal_coerces_against_range(storage, session):
    # PG infers an untyped literal's type from the other operand.
    assert run(storage, session, "SELECT 'empty' = 'empty'::int4range").rows == [(True,)]
    assert run(storage, session, "SELECT '{empty}' = array['empty'::int4range]").rows == [(True,)]


def test_range_array_cast_coerces_elements(storage, session):
    r = run(
        storage,
        session,
        "SELECT array['empty'::int4range, '(,)'::int4range] = '{empty,\"(,)\"}'::int4range[]",
    )
    assert r.rows == [(True,)]
    r = run(storage, session, "SELECT ('{\"[1,3)\"}'::int4range[])[1]")
    assert r.rows == [({"lower": 1, "upper": 3, "lower_inc": True, "upper_inc": False},)]


def test_range_cast_to_text_renders_literal(storage, session):
    assert run(storage, session, "SELECT '[1,3)'::int4range::text").rows == [("[1,3)",)]
    assert run(storage, session, "SELECT '{[1,3)}'::int4multirange::text").rows == [("{[1,3)}",)]
