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


def test_binary_numeric_roundtrip_wide_integral_and_fractional():
    # A wide integral value with a large declared scale used to make the
    # decoder's quantize raise InvalidOperation (an under-sized context).
    for s in ("1" + "0" * 50, "9" * 60, "1" + "0" * 50 + "." + "0" * 50, "0." + "0" * 45 + "1"):
        d = Decimal(s)
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


def test_untyped_binary_param_non_text_is_22P03():
    # A binary payload for an untyped (oid 0) parameter that isn't valid text —
    # e.g. an EWKB GEOMETRY value for a type we don't model — surfaces a
    # faithful 22P03, not a leaked UnicodeDecodeError (generic XX000).
    from secantus.sql import errors

    ewkb = bytes.fromhex("0101000020E6100000000000000000F03F000000000000F03F")
    with pytest.raises(errors.SQLError) as e:
        pgextended._decode_param(ewkb, 1, 0)
    assert e.value.sqlstate == "22P03"


def test_length_qualified_char_casts_truncate_and_pad():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")

        def one(sql):
            r = run_sql(st, "d", sql, session=sess)[-1]
            return r.rows[0][0], r.columns[0].pg_oid, r.columns[0].typmod

        # varchar(n) truncates the value; identity is varchar (1043), typmod n+4.
        assert one("SELECT 'bar'::VARCHAR(2)") == ("ba", 1043, 6)
        # char(n) truncates AND blank-pads to n.
        assert one("SELECT 'bar'::CHAR(2)") == ("ba", 1042, 6)
        assert one("SELECT 'a'::CHAR(4)") == ("a   ", 1042, 8)
        # Bare text/varchar impose no limit.
        assert one("SELECT 'foobar'::TEXT") == ("foobar", 25, -1)
        assert one("SELECT 'foobar'::VARCHAR") == ("foobar", 1043, -1)
    finally:
        st.close()


def test_three_valued_logic_in_per_row_where():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table tv (a int4, b int4)", session=sess)
        run_sql(st, "d", "insert into tv values (1, 2), (5, 6)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # NOT over an unknown is unknown — the row is excluded, not included.
        assert rows("select a from tv where a not between (null) and b") == []
        # ...but a definitively-false arm dominates the NULL bound.
        assert rows("select a from tv where b not between null and a") == [(1,), (5,)]
        # Untranslatable-WHERE shapes route to per-row evaluation.
        assert rows("select 1 from tv where 1 in (2)") == []
        assert rows("select a from tv where - b + a > - 10") == [(1,), (5,)]
        assert rows("select distinct a from tv where - b + a is not null") == [(1,), (5,)]
        # Computed unary projections type and evaluate.
        assert rows("select - a as neg from tv order by neg") == [(-5,), (-1,)]
        assert rows("select + + 90 * a * - b from tv order by 1") == [(-2700,), (-180,)]
    finally:
        st.close()


def test_aggregate_expression_arguments():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table ag (col0 int4, col1 int4)", session=sess)
        run_sql(st, "d", "insert into ag values (46, 1), (64, 3), (75, 5)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # Identity-wrapped args strip; real expressions lower to agg exprs.
        assert rows("select - max( - - col0 ) from ag") == [(-75,)]
        assert rows("select sum( all - 83 ) from ag") == [(-249,)]
        assert rows("select sum( distinct - - ( col1 ) ) from ag") == [(9,)]
        assert rows("select - cast( + - sum( - col1 ) as integer ) from ag") == [(-9,)]
        assert rows("select max(col0 + 1) from ag") == [(76,)]
        assert rows("select count(*), sum(col1 + col0) from ag") == [(3, 194)]
    finally:
        st.close()


def test_empty_implicit_aggregate_returns_one_row():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table em (a int4)", session=sess)
        run_sql(st, "d", "insert into em values (1)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # PG: an implicit whole-table aggregate over zero rows is ONE row —
        # counts 0, everything else NULL. A grouped query stays zero rows.
        assert rows("select avg(a) from em where a > 99") == [(None,)]
        assert rows("select sum(a), max(a) from em where a > 99") == [(None, None)]
        assert rows("select count(*), avg(a) from em where a > 99") == [(0, None)]
        assert rows("select a from em where a > 99 group by a") == []
        # Comparison with a bare NULL literal matches nothing on the pushdown.
        assert rows("select a from em where a <> NULL") == []
        assert rows("select 1 from em where NULL NOT IN ()") == [(1,)]
    finally:
        st.close()


def test_join_aggregate_expression_args_and_where_residual():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table j0 (col0 int4)", session=sess)
        run_sql(st, "d", "create table j1 (col0 int4)", session=sess)
        run_sql(st, "d", "insert into j0 values (1), (2)", session=sess)
        run_sql(st, "d", "insert into j1 values (3)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # Expression aggregate args resolve through the join resolver.
        assert rows("select max(cor0.col0 + 1) from j0 cor0 cross join j1") == [(3,)]
        # A WHERE the join $match can't lower routes to the per-row residual.
        assert rows(
            "select sum( all - 83 ) from j0 cor0 cross join j0 cor1 where not ( 15 ) is null"
        ) == [(-332,)]
        assert rows(
            "select cor0.col0 from j0 cor0 cross join j1 where - cor0.col0 + 3 > 1 order by 1"
        ) == [(1,)]
        # Computed-over-aggregate outputs route to the group-then-evaluate
        # builder even without GROUP BY or windows.
        assert rows("select count(*) * 32 from j1 cross join j0") == [(64,)]
        assert rows("select count(*) + sum(cor0.col0) from j0 cor0 cross join j1") == [(5,)]
    finally:
        st.close()


def test_fromless_aggregates_fold_over_one_row():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # PG feeds a FROM-less aggregation exactly one implicit row.
        assert rows("select count(*)") == [(1,)]
        assert rows("select all + count( * ) as col0") == [(1,)]
        assert rows("select count(22), count(null)") == [(1, 0)]
        assert rows("select sum(distinct 73), min(all -32), avg(5)") == [(73, -32, 5)]
        assert rows("select nullif( - count( * ), 67 ) + 54") == [(53,)]
        # An UNGROUPED aggregate yields exactly one row even when the WHERE
        # excludes the implicit input row — COUNT is 0, the rest NULL. (This
        # line previously asserted zero rows; verified against real PostgreSQL
        # 14.13: ``select max(3) where 1=2`` returns one NULL row, and
        # ``select 0/count(*) where 1=2`` therefore raises division_by_zero,
        # which is how pgjdbc's batch tests inject a runtime failure.)
        assert rows("select max(3) where 1 = 2") == [(None,)]
        assert rows("select count(*) where 1 = 2") == [(0,)]
        assert rows("select sum(1) where 1 = 2") == [(None,)]
        # A non-aggregate projection still yields zero rows.
        assert rows("select 1 where 1 = 2") == []
    finally:
        st.close()


def test_empty_aggregate_row_on_evaluated_group_path():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table ev (col1 int4, col2 int4)", session=sess)
        run_sql(st, "d", "insert into ev values (2, 3)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # Computed-over-aggregate outputs (the group-then-evaluate path) still
        # synthesize the one whole-table-aggregate row over zero input rows.
        assert rows("select - avg( - - col1 ) from ev where null = col1 + 69") == [(None,)]
        assert rows(
            "select - count( * ) / 47 from ev where not col2 not in ( cast(null as real) )"
        ) == [(0,)]
        assert rows("select count(*) * 32 from ev where col1 > 99") == [(0,)]
    finally:
        st.close()


def test_group_by_computed_projections():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table gp (col0 int4, col2 int4)", session=sess)
        run_sql(st, "d", "insert into gp values (1, 7), (4, 7)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # Arbitrary expressions over group keys (and bare constants) project.
        assert rows("select - col0 * 84 + 38 from gp group by col0 order by 1") == [
            (-298,),
            (-46,),
        ]
        assert rows("select distinct - 53 from gp group by col2") == [(-53,)]
        assert rows(
            "select case when col0 > 2 then 1 else 0 end from gp group by col0 order by 1"
        ) == [
            (0,),
            (1,),
        ]
    finally:
        st.close()


def test_parenthesized_join_from():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table pj (col0 int4)", session=sess)
        run_sql(st, "d", "insert into pj values (1), (2)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # FROM-parens are grouping, not a derived table needing an alias.
        assert rows("select count(*) from ( pj as cor0 cross join pj cor1 )") == [(4,)]
        assert rows(
            "select min(all - 32), count(*) * count(*) from ( pj as cor0 cross join pj cor1 )"
        ) == [(-32, 16)]
    finally:
        st.close()


def test_constant_lhs_in_subquery():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table ci (x int4)", session=sess)
        run_sql(st, "d", "insert into ci values (1)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        assert rows("select 1 from ci where 1 in (select 1)") == [(1,)]
        assert rows("select 1 from ci where 1 in (select 2)") == []
        assert rows("select 1 from ci where 1 not in (select 2)") == [(1,)]
        assert rows("select 1 from ci where null in (select 1)") == []
        assert rows("select 1 from ci where 1 not in (select null)") == []
    finally:
        st.close()


def test_three_valued_null_semantics_on_pushdown():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table tn (a int4, d int4)", session=sess)
        run_sql(st, "d", "insert into tn values (1, 120), (2, 200), (3, null)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # A NULL operand makes these unknown — excluded, never matched. Mongo's
        # bare $ne / $nor / $nin / $in-with-None would include the NULL row.
        assert rows("select a from tn where d <> 120") == [(2,)]
        assert rows("select a from tn where not (d = 120)") == [(2,)]
        assert rows("select a from tn where d not between 110 and 150") == [(2,)]
        assert rows("select a from tn where d not in (120, 121)") == [(2,)]
        assert rows("select a from tn where d not in (120, null)") == []
        assert rows("select a from tn where d in (200, null)") == [(2,)]
        assert rows("select a from tn where not (d < 130)") == [(2,)]
        # De Morgan: a definitively-false conjunct rescues the NULL row.
        assert rows("select a from tn where not (a = 1 and d = 120) order by a") == [(2,), (3,)]
        assert rows("select a from tn where not (a = 9 or d = 120) order by a") == [(2,)]
        # NOT over a pattern match is null-guarded.
        assert rows("select a from tn where not (cast(d as text) like '12%')") == [(2,)]
    finally:
        st.close()


def test_float8_wire_text_rendering():
    assert typemap.to_pg_text(12.0, "float8") == b"12"
    assert typemap.to_pg_text(-0.0, "float8") == b"-0"
    assert typemap.to_pg_text(0.5, "float8") == b"0.5"
    assert typemap.to_pg_text(1e20, "float8") == b"1e+20"
    assert typemap.to_pg_text(float("nan"), "float8") == b"NaN"
    assert typemap.to_pg_text(float("inf"), "float8") == b"Infinity"
    assert typemap.to_pg_text(float("-inf"), "float8") == b"-Infinity"
    assert typemap.to_pg_text([1.0, 2.5], "float8[]") == b"{1,2.5}"


def test_count_and_distinct_expression_aggregates():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table ce (col0 int4)", session=sess)
        run_sql(st, "d", "insert into ce values (1), (2), (3)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # COUNT over a literal is not COUNT(*) — it counts non-null evaluations.
        assert rows("select count(73) from ce") == [(3,)]
        assert rows("select count(null) from ce") == [(0,)]
        assert rows("select count(null) from ce cor0 cross join ce cor1") == [(0,)]
        # DISTINCT over expression arguments.
        assert rows("select sum(distinct 77) from ce") == [(77,)]
        assert rows("select count(distinct 44) from ce") == [(1,)]
        assert rows("select distinct - count(distinct 74) from ce cor0 cross join ce cor1") == [
            (-1,)
        ]
    finally:
        st.close()


def test_sum_over_all_null_is_null():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table sn (a int4, g int4)", session=sess)
        run_sql(st, "d", "insert into sn values (null, 1), (null, 1)", session=sess)
        run_sql(st, "d", "create table sj (x int4)", session=sess)
        run_sql(st, "d", "insert into sj values (7)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # PG: SUM with zero non-null inputs is NULL, not Mongo's $sum 0.
        assert rows("select sum(a) from sn") == [(None,)]
        assert rows("select sum(- cast(null as integer)) from sn") == [(None,)]
        assert rows("select sum(distinct a) from sn") == [(None,)]
        assert rows("select sum(a) from sn group by g") == [(None,)]
        assert rows("select sum(sn.a) from sn cross join sj") == [(None,)]
        assert rows("select sum(sn.a) + 1 from sn cross join sj") == [(None,)]
    finally:
        st.close()


def test_grouped_star_and_distinct_dedup():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table gs (col0 int4, col1 int4)", session=sess)
        run_sql(st, "d", "insert into gs values (83, 0), (26, 0), (43, 81)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # SELECT * under GROUP BY over every column expands to the group keys.
        assert sorted(rows("select distinct * from gs group by col1, col0")) == [
            (26, 0),
            (43, 81),
            (83, 0),
        ]
        # SELECT DISTINCT over grouped output dedups the projected rows.
        assert sorted(rows("select distinct col1 from gs group by col1, col0")) == [(0,), (81,)]
    finally:
        st.close()


def test_division_by_zero_sqlstate():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        with pytest.raises(Exception) as exc:
            run_sql(st, "d", "select 1 / 0", session=sess)
        assert getattr(exc.value, "sqlstate", None) == "22012"
        with pytest.raises(Exception) as exc:
            run_sql(st, "d", "select 1 % 0", session=sess)
        assert getattr(exc.value, "sqlstate", None) == "22012"
    finally:
        st.close()


def test_join_where_residual_not_dropped():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table jw (a int4)", session=sess)
        run_sql(st, "d", "insert into jw values (1), (2)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # A WHERE the join $match can't lower must filter per-row, not vanish.
        assert rows("select * from jw, jw cor0 where ( null ) between null and null") == []
    finally:
        st.close()


def test_lazy_coalesce_and_constant_having():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table ch (col2 int4)", session=sess)
        run_sql(st, "d", "insert into ch values (1), (2)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # COALESCE is lazy: arms after the first non-null never evaluate.
        assert rows("select coalesce(-14, 1/0, -93)") == [(-14,)]
        assert rows("select coalesce(-14, 1/0) from ch") == [(-14,), (-14,)]
        # Constant HAVING folds three-valued: unknown excludes every group.
        assert rows("select col2 from ch group by col2 having not null is null") == []
        assert rows("select col2 from ch group by col2 having not null < null") == []
        assert len(rows("select col2 from ch group by col2 having null is null")) == 2
        # DISTINCT aggregates over zero rows synthesize NULL, not a crash.
        assert rows("select sum(distinct col2) from ch where 1 = 2") == [(None,)]
        assert rows("select avg(distinct col2) from ch where null is not null") == [(None,)]
    finally:
        st.close()


def test_operand_case_null_never_matches():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table oc (a int4, e int4)", session=sess)
        run_sql(st, "d", "insert into oc values (null, null), (1, 2)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # Operand-form CASE uses SQL equality: NULL never matches NULL.
        assert rows(
            "select case a + 1 when e then 444 else 555 end from oc order by a nulls first"
        ) == [(555,), (444,)]
    finally:
        st.close()


def test_expression_aggregates_do_not_share_accumulators():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table ea (col1 int4)", session=sess)
        run_sql(st, "d", "insert into ea values (81)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # Two expression aggregates of the same function must not collide on
        # the (func, None) dedup key and share one accumulator.
        assert rows("select - 62 + max(3) * max(- 94 - - 16) from ea") == [(-296,)]
        # ...while identical aggregates still dedup to one accumulator.
        assert rows("select max(col1) + max(col1) from ea") == [(162,)]
    finally:
        st.close()


def test_integer_division_truncates_in_aggregate_args():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table dv (col1 int4)", session=sess)
        run_sql(st, "d", "insert into dv values (81)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # PG integer '/' truncates toward zero; Mongo's $divide is real division.
        assert rows("select min(col1 / -99) from dv") == [(0,)]
        assert rows("select min(col1 / 2) from dv") == [(40,)]
        assert rows("select min(col1 / 2.0) from dv") == [(40.5,)]
    finally:
        st.close()


def test_join_group_window_where_residual_not_dropped():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table jg (a int4)", session=sess)
        run_sql(st, "d", "insert into jg values (1), (2), (3)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # A WHERE the join $match can't lower still filters before the $group
        # on the computed-over-aggregate (group-window) path.
        assert rows(
            "select count(*) + 93 from ( jg as cor0 cross join jg cor1 ) where not null is null"
        ) == [(93,)]
    finally:
        st.close()


def test_wrapped_null_literal_comparisons_match_nothing():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table wn (col1 int4)", session=sess)
        run_sql(st, "d", "insert into wn values (51), (67)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # NULL comparison operands fold to match-nothing even when wrapped
        # (parens, negation, cast) — the $expr lowering's BSON-order compare
        # would otherwise match rows.
        assert rows("select col1 from wn where 51 <> ( null )") == []
        assert rows("select col1 from wn where ( null ) <> 89 * 41 + col1") == []
        assert rows("select col1 from wn where - cast ( null as integer ) <> 90 * col1") == []
        assert rows("select col1 from wn where not (51 = ( null ))") == []
    finally:
        st.close()


def test_computed_comparison_null_operand_excluded():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table cc (col1 int4, col2 int4)", session=sess)
        run_sql(st, "d", "insert into cc values (5, 7), (2, 3)", session=sess)

        def rows(sql):
            return run_sql(st, "d", sql, session=sess)[-1].rows

        # A computed comparison whose side evaluates NULL is unknown — excluded.
        # (The $expr lowering's BSON-order compare would match: NULL <> 19.)
        assert (
            rows("select col1 from cc where col2 + 76 / (cast(null as integer) + col1) <> 19") == []
        )
        assert rows("select col1 from cc where col1 + col2 > 8") == [(5,)]
        assert sorted(rows("select col1 from cc where col1 * 2 <> col2")) == [(2,), (5,)]
    finally:
        st.close()


def test_having_is_null_forms():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table hn (col1 int4, col2 int4)", session=sess)
        run_sql(st, "d", "insert into hn values (1, 10), (2, null)", session=sess)

        def rows(sql):
            return sorted(run_sql(st, "d", sql, session=sess)[-1].rows)

        # Bare, aggregate, and computed operands; direct and negated forms.
        assert rows("select col2 from hn group by col2 having col2 is null") == [(None,)]
        assert rows("select col2 from hn group by col2 having col2 is not null") == [(10,)]
        assert rows("select sum(col2) from hn group by col1 having sum(col2) is not null") == [
            (10,)
        ]
        assert rows("select col2 + col2 from hn group by col2 having ( - col2 ) is not null") == [
            (20,)
        ]
        assert rows("select col1 from hn group by col1 having not ( col1 + col1 ) is null") == [
            (1,),
            (2,),
        ]
        # The join HAVING lowerer mirrors the same forms.
        assert rows(
            "select cor0.col2 from hn cor0 cross join hn cor1 "
            "group by cor0.col2 having (- cor0.col2) is not null"
        ) == [(10,)]
    finally:
        st.close()


def test_constant_join_on():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table ca (a int4)", session=sess)
        run_sql(st, "d", "create table cb (b int4)", session=sess)
        run_sql(st, "d", "insert into ca values (1), (2)", session=sess)
        run_sql(st, "d", "insert into cb values (7)", session=sess)

        def rows(sql):
            return sorted(run_sql(st, "d", sql, session=sess)[-1].rows)

        # Constant ON folds three-valued: FALSE/unknown null-pads a LEFT JOIN
        # (one row per left row) and empties an INNER; TRUE is the cartesian.
        assert rows("select 11 from cb left join ca on 80 = 70") == [(11,)]
        assert rows("select b, a from cb left join ca on false") == [(7, None)]
        assert rows("select 11 from cb join ca on 80 = 70") == []
        assert rows("select count(*) from cb join ca on null") == [(0,)]
        assert rows("select 11 from cb left join ca on 80 = 80") == [(11,), (11,)]
    finally:
        st.close()


def test_join_group_by_duplicate_column_names():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table dj (col0 int4, col1 int4)", session=sess)
        run_sql(st, "d", "insert into dj values (22, 6), (28, 57), (82, 44)", session=sess)

        def rows(sql):
            return sorted(run_sql(st, "d", sql, session=sess)[-1].rows)

        # GROUP BY the same bare column name from two aliases must group by
        # BOTH (9 groups over the 3x3 cross join), not collapse to one key.
        assert (
            rows("select cor0.col1 from dj cor0 cross join dj cor1 group by cor1.col1, cor0.col1")
            == [(6,)] * 3 + [(44,)] * 3 + [(57,)] * 3
        )
        # ...on the computed-projection (group-window) path too.
        assert (
            rows(
                "select 50 + - cor0.col1 from dj cor0 cross join dj cor1 "
                "group by cor1.col1, cor0.col1"
            )
            == [(-7,)] * 3 + [(6,)] * 3 + [(44,)] * 3
        )
        # Both duplicate keys project side by side.
        assert (
            len(
                rows(
                    "select cor0.col1, cor1.col1 from dj cor0 cross join dj cor1 "
                    "group by cor1.col1, cor0.col1"
                )
            )
            == 9
        )
    finally:
        st.close()


def test_join_grouped_distinct_dedup():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table ga (col1 int4)", session=sess)
        run_sql(st, "d", "create table gb (col0 int4)", session=sess)
        run_sql(st, "d", "insert into ga values (0), (0), (81)", session=sess)
        run_sql(st, "d", "insert into gb values (22), (28)", session=sess)

        def rows(sql):
            return sorted(run_sql(st, "d", sql, session=sess)[-1].rows)

        # DISTINCT over a subset of the join's group keys dedups.
        assert rows(
            "select distinct cor1.col0 from ga cor0 cross join gb cor1 "
            "group by cor0.col1, cor1.col0"
        ) == [(22,), (28,)]
    finally:
        st.close()


def test_having_null_operand_and_in_over_group_keys():
    st = Storage(":memory:")
    try:
        sess = Session(database="d")
        run_sql(st, "d", "create table hg (col0 int4, col2 int4)", session=sess)
        run_sql(st, "d", "insert into hg values (1, 10), (2, null)", session=sess)

        def rows(sql):
            return sorted(run_sql(st, "d", sql, session=sess)[-1].rows)

        # Always-unknown NULL-operand predicates exclude every group, through
        # any NOT nesting.
        assert rows("select col0 from hg group by col0 having not null in ( - col0 )") == []
        assert rows("select col0 from hg group by col0 having not null = col0") == []
        assert (
            rows("select col0 from hg group by col0 having not null not between - col0 and null")
            == []
        )
        # Doubly-negated IS NULL flips back.
        assert rows("select col2 from hg group by col2 having not col2 is not null") == [(None,)]
        # Membership over group keys, three-valued: NOT x IN (x) never holds,
        # x IN (x) holds only for non-null keys.
        assert rows("select col0 from hg group by col0 having not col0 in ( col0 )") == []
        assert rows("select col2 from hg group by col2 having col2 in ( col2 )") == [(10,)]
        # An always-unknown JOIN ON never matches: LEFT null-pads per left row.
        assert rows(
            "select cor0.col0 from hg cor0 left join hg cor1 on not null < - cor0.col0 "
            "group by cor0.col0"
        ) == [(1,), (2,)]
    finally:
        st.close()
