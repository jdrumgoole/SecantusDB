"""Regression tests for the psycopg-faker type-fidelity fixes.

Covers: text-format bool coercion, JSON big-int storage/rendering, Postgres
binary parameter decode / result encode for the non-core types (time / timetz /
interval / uuid / inet / cidr / macaddr / numeric / ranges / multiranges), the
tstzrange / tstzmultirange registration, the ``oid`` type (including the
``oid[]`` DDL rewrite), and Decimal128-safe range comparisons — driven against
the real WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import datetime as dt
import struct
import uuid
from decimal import Decimal

import bson
import pytest

from secantus.sql import pgextended, ranges, typemap
from secantus.sql.engine import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture
def storage(tmp_path):
    st = Storage(str(tmp_path))
    try:
        yield st
    finally:
        st.close()


@pytest.fixture
def session():
    return Session()


# --------------------------------------------------------------------------- #
# coerce
# --------------------------------------------------------------------------- #


def test_coerce_bool_text_spellings():
    assert typemap.coerce("t", "bool") is True
    assert typemap.coerce("f", "bool") is False
    assert typemap.coerce("false", "bool") is False
    assert typemap.coerce("TRUE", "bool") is True
    with pytest.raises(ValueError):
        typemap.coerce("maybe", "bool")


def test_coerce_json_bigint_roundtrips_as_decimal128():
    big = 614798960736101077805736138  # > int64: BSON can't store it as an int
    stored = typemap.coerce(f'{{"a": {big}}}', "json")
    assert isinstance(stored["a"], bson.Decimal128)
    assert typemap.to_pg_text(stored, "json") == f'{{"a": {big}}}'.encode()


def test_json_scalar_renders_as_json_text():
    # A bare JSON string / bool must render as JSON, not the text/bool forms.
    assert typemap.to_pg_text("abc", "json") == b'"abc"'
    assert typemap.to_pg_text(True, "json") == b"true"
    assert typemap.to_pg_text(Decimal(10) ** 20, "json") == b"100000000000000000000"


def test_coerce_numeric_beyond_decimal128_rounds():
    d = Decimal("1" * 40)  # 40 significant digits — Decimal128 holds 34
    got = typemap.coerce(d, "numeric").to_decimal()
    assert got == Decimal("1.111111111111111111111111111111111E+39")


# --------------------------------------------------------------------------- #
# ranges
# --------------------------------------------------------------------------- #


def test_numrange_decimal128_bounds_compare():
    lo, hi = bson.Decimal128("1.5"), bson.Decimal128("2.5")
    r = ranges.make_range(lo, hi, "[)", "numrange")
    assert ranges.render(r, "numrange") == "[1.5,2.5)"
    assert ranges.contains_value(r, bson.Decimal128("2.0"))
    # equal-bound exclusive collapses to empty (Decimal128 comparison path)
    assert ranges.make_range(lo, lo, "[)", "numrange") == {"empty": True}


def test_multirange_renders_without_separator_space():
    mr = ranges.make_multirange(
        [ranges.make_range(1, 5, "[)", "int4range"), ranges.make_range(10, 20, "[)", "int4range")]
    )
    assert ranges.render_multirange(mr, "int4multirange") == "{[1,5),[10,20)}"


def test_daterange_renders_date_only_bounds():
    r = ranges.make_range(dt.datetime(2021, 3, 4), dt.datetime(2021, 3, 9), "[)", "daterange")
    assert ranges.render(r, "daterange") == "[2021-03-04,2021-03-09)"


def test_tstzrange_registered_and_renders_offset():
    assert "tstzrange" in ranges.RANGE_TYPES
    assert ranges.MULTIRANGE_TYPES["tstzmultirange"] == "tstzrange"
    assert typemap.PG_OID["tstzrange"] == 3910
    assert typemap.PG_OID["tstzmultirange"] == 4534
    r = ranges.make_range(dt.datetime(2021, 1, 1), dt.datetime(2021, 1, 2), "[)", "tstzrange")
    # stored bounds decode tz-naive UTC from BSON -> render tags them UTC
    assert (
        ranges.render(r, "tstzrange") == '["2021-01-01 00:00:00+00:00","2021-01-02 00:00:00+00:00")'
    )


def test_tsrange_bounds_quoted():
    r = ranges.make_range(dt.datetime(2021, 1, 1), dt.datetime(2021, 1, 2), "[)", "tsrange")
    text = ranges.render(r, "tsrange")
    assert text == '["2021-01-01 00:00:00","2021-01-02 00:00:00")'
    # and the text form parses back
    back = ranges.parse_literal(text, "tsrange", lambda t: typemap.coerce(t, "timestamptz"))
    assert back["lower"] == dt.datetime(2021, 1, 1)


# --------------------------------------------------------------------------- #
# binary parameter decode (Postgres wire binary -> canonical text/value)
# --------------------------------------------------------------------------- #


def test_binary_decode_time_and_timetz():
    micros = ((1 * 60 + 2) * 60 + 3) * 1_000_000 + 123456
    assert pgextended._BINARY[1083](struct.pack("!q", micros)) == "01:02:03.123456"
    b = struct.pack("!qi", micros, -3600)  # zone: 1h east of UTC
    assert pgextended._BINARY[1266](b) == "01:02:03.123456+01:00"


def test_binary_decode_interval_roundtrip():
    b = struct.pack("!qii", 3723 * 1_000_000, 5, 14)
    text = pgextended._BINARY[1186](b)
    assert pgextended._encode_interval(text) == b


def test_binary_decode_inet_uuid_macaddr():
    b = bytes([2, 24, 0, 4, 10, 1, 2, 0])
    assert pgextended._BINARY[869](b) == "10.1.2.0/24"
    u = uuid.uuid4()
    assert pgextended._BINARY[2950](u.bytes) == str(u)
    assert pgextended._BINARY[829](bytes.fromhex("0800275aabbc")) == "08:00:27:5a:ab:bc"


def test_binary_decode_numeric_long_precision():
    d = Decimal("-0.12345678901234567890123456789012345e-15")
    assert pgextended._decode_numeric(pgextended._encode_numeric(d)) == d


def test_binary_range_and_multirange_roundtrip():
    r = ranges.make_range(1, 5, "[)", "int4range")
    rb = pgextended._encode_range(r, 3904)
    assert pgextended._decode_range(rb, 23) == "[1,5)"
    mr = ranges.make_multirange(
        [ranges.make_range(1, 5, "[)", "int4range"), ranges.make_range(10, 20, "[)", "int4range")]
    )
    mb = pgextended._encode_multirange(mr, 4451)
    assert pgextended._decode_multirange(mb, 3904) == "{[1,5),[10,20)}"


def test_binary_encode_date_from_stored_text():
    # ``date`` is stored as canonical text; the binary encoder must parse it.
    assert pgextended._encode_date("2000-01-11") == struct.pack("!i", 10)


def test_binary_encode_json_array_string_element():
    # A stored json[] element that IS a JSON string must not be re-parsed.
    out = pgextended._encode_array(["abc"], 3807, "json[]")
    assert b'"abc"' in out


# --------------------------------------------------------------------------- #
# end-to-end against the real Storage
# --------------------------------------------------------------------------- #


def test_oid_column_end_to_end(storage, session):
    run_sql(
        storage, "db", "create table t_oid (id serial primary key, a oid, b oid[])", session=session
    )
    run_sql(storage, "db", "insert into t_oid (a, b) values (4294967295, '{1,2}')", session=session)
    res = run_sql(storage, "db", "select a, b from t_oid", session=session)[0]
    assert res.rows == [(4294967295, [1, 2])]


def test_quoted_oid_array_ddl(storage, session):
    # psycopg's faker emits ``"oid"[]`` — sqlglot can't parse the OID keyword
    # with an array suffix, so planner.parse rewrites it.
    run_sql(storage, "db", 'create table t_oid2 (a "oid", b "oid"[])', session=session)
    run_sql(storage, "db", "insert into t_oid2 (a, b) values (7, '{3}')", session=session)
    res = run_sql(storage, "db", "select a, b from t_oid2", session=session)[0]
    assert res.rows == [(7, [3])]


def test_tstzrange_column_end_to_end(storage, session):
    run_sql(storage, "db", "create table t_tz (r tstzrange)", session=session)
    run_sql(
        storage,
        "db",
        "insert into t_tz values ('[2021-01-01 00:00:00+00,2021-01-02 00:00:00+00)')",
        session=session,
    )
    res = run_sql(storage, "db", "select r from t_tz", session=session)[0]
    assert res.rows[0][0]["lower"].replace(tzinfo=None) == dt.datetime(2021, 1, 1)


def test_bool_text_param_false_end_to_end(storage, session):
    run_sql(storage, "db", "create table t_b (v bool)", session=session)
    run_sql(storage, "db", "insert into t_b values ('f')", session=session)
    res = run_sql(storage, "db", "select v from t_b", session=session)[0]
    assert res.rows == [(False,)]


def test_json_bigint_column_end_to_end(storage, session):
    big = 614798960736101077805736138
    run_sql(storage, "db", "create table t_j (v jsonb)", session=session)
    run_sql(storage, "db", f"""insert into t_j values ('{{"a": {big}}}')""", session=session)
    res = run_sql(storage, "db", "select v from t_j", session=session)[0]
    assert typemap.to_pg_text(res.rows[0][0], "json") == f'{{"a": {big}}}'.encode()


def test_infinite_float_param_node():
    from sqlglot import exp

    from secantus.sql import planner

    for v, want in [(float("inf"), "Infinity"), (float("-inf"), "-Infinity")]:
        node = planner._value_to_node(v)
        # Carried as a float8 cast around the Postgres spelling (the cast
        # evaluator converts it back to a float).
        assert isinstance(node, exp.Cast)
        assert node.this.is_string and node.this.this == want
    nan = planner._value_to_node(float("nan"))
    assert isinstance(nan, exp.Cast)
    assert nan.this.is_string and nan.this.this == "NaN"


def test_untyped_empty_multirange_binary_param():
    assert pgextended._decode_param(b"\x00\x00\x00\x00", 1, 0) == "{}"
    assert typemap.coerce("{}", "nummultirange") == {"multirange": []}
