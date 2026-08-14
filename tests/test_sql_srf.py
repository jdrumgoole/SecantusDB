"""generate_series + base-less FROM-clause set-returning functions (#125):
``generate_series`` (SELECT-list and FROM forms), ``FROM unnest(...)`` /
``jsonb_array_elements`` / ``regexp_split_to_table``, ``WITH ORDINALITY``, and
column/table aliases — all with projection / WHERE / ORDER BY / LIMIT.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


def _run(sql, sess=None, st=None):
    sess = sess or Session(database=DB)
    if st is not None:
        return run_sql(st, DB, sql, session=sess)[-1]
    st = Storage(":memory:")
    try:
        return run_sql(st, DB, sql, session=sess)[-1]
    finally:
        st.close()


def _rows(sql):
    return _run(sql).rows


def _cols(sql):
    r = _run(sql)
    return [c.name for c in r.columns]


# --------------------------------------------------------------------------- #
# generate_series
# --------------------------------------------------------------------------- #


def test_generate_series_fromless():
    assert _rows("SELECT generate_series(1, 5)") == [(1,), (2,), (3,), (4,), (5,)]
    assert _cols("SELECT generate_series(1, 5)") == ["generate_series"]


def test_generate_series_from():
    assert _rows("SELECT * FROM generate_series(1, 4)") == [(1,), (2,), (3,), (4,)]


def test_generate_series_step():
    assert _rows("SELECT * FROM generate_series(1, 10, 2)") == [(1,), (3,), (5,), (7,), (9,)]


def test_generate_series_negative_step():
    assert _rows("SELECT * FROM generate_series(5, 1, -2)") == [(5,), (3,), (1,)]


def test_generate_series_empty_and_single():
    assert _rows("SELECT * FROM generate_series(5, 1)") == []
    assert _rows("SELECT * FROM generate_series(3, 3)") == [(3,)]


def test_generate_series_zero_step_errors():
    with pytest.raises(errors.SQLError) as exc:
        _rows("SELECT * FROM generate_series(1, 5, 0)")
    assert exc.value.sqlstate == "22023"


def test_generate_series_count():
    assert _rows("SELECT count(*) FROM generate_series(1, 100)") == [(100,)]


# --------------------------------------------------------------------------- #
# Aliases, WHERE / ORDER BY / LIMIT
# --------------------------------------------------------------------------- #


def test_table_alias_names_column():
    # FROM generate_series(1,5) AS g -> the single column is named g.
    assert _cols("SELECT * FROM generate_series(1, 3) AS g") == ["g"]
    assert _rows("SELECT g FROM generate_series(1, 3) AS g") == [(1,), (2,), (3,)]


def test_column_alias():
    assert _cols("SELECT * FROM generate_series(1, 3) AS g(n)") == ["n"]
    assert _rows("SELECT n FROM generate_series(1, 3) AS g(n) WHERE n > 1") == [(2,), (3,)]


def test_order_by_and_limit():
    assert _rows("SELECT * FROM generate_series(1, 5) AS g ORDER BY g DESC LIMIT 2") == [(5,), (4,)]


# --------------------------------------------------------------------------- #
# WITH ORDINALITY
# --------------------------------------------------------------------------- #


def test_with_ordinality():
    r = _run("SELECT * FROM generate_series(10, 30, 10) WITH ORDINALITY")
    assert [c.name for c in r.columns] == ["generate_series", "ordinality"]
    assert r.rows == [(10, 1), (20, 2), (30, 3)]


def test_with_ordinality_aliased():
    r = _run("SELECT * FROM generate_series(1, 3) WITH ORDINALITY AS t(v, ord)")
    assert [c.name for c in r.columns] == ["v", "ord"]
    assert r.rows == [(1, 1), (2, 2), (3, 3)]


# --------------------------------------------------------------------------- #
# Other FROM-clause SRFs
# --------------------------------------------------------------------------- #


def test_from_unnest():
    assert _rows("SELECT * FROM unnest(ARRAY[10, 20, 30]) AS x") == [(10,), (20,), (30,)]
    assert _cols("SELECT * FROM unnest(ARRAY[10, 20, 30]) AS x") == ["x"]


def test_from_regexp_split_to_table():
    assert _rows("SELECT * FROM regexp_split_to_table('a,b,c', ',') AS p") == [
        ("a",),
        ("b",),
        ("c",),
    ]


def test_from_jsonb_array_elements():
    assert _rows("SELECT * FROM jsonb_array_elements('[1,2,3]'::jsonb) AS e") == [(1,), (2,), (3,)]


def test_from_jsonb_object_keys():
    got = _rows('SELECT * FROM jsonb_object_keys(\'{"a":1,"b":2}\'::jsonb) AS k')
    assert sorted(r[0] for r in got) == ["a", "b"]


# --------------------------------------------------------------------------- #
# Errors
# --------------------------------------------------------------------------- #


def test_generate_series_timestamp_step_zero_rejected():
    sql = (
        "SELECT * FROM generate_series(timestamp '2026-01-01', "
        "timestamp '2026-01-03', interval '0 day')"
    )
    with pytest.raises(errors.SQLError) as exc:
        _rows(sql)
    assert exc.value.sqlstate == "22023"


# --------------------------------------------------------------------------- #
# generate_series over timestamps + interval step (#150)
# --------------------------------------------------------------------------- #

import datetime as _dt  # noqa: E402


def test_generate_series_timestamp_day_step():
    rows = _rows(
        "SELECT * FROM generate_series(timestamp '2024-01-01', "
        "timestamp '2024-01-03', interval '1 day')"
    )
    assert rows == [
        (_dt.datetime(2024, 1, 1),),
        (_dt.datetime(2024, 1, 2),),
        (_dt.datetime(2024, 1, 3),),
    ]


def test_generate_series_timestamp_sub_day_step():
    rows = _rows(
        "SELECT generate_series(timestamp '2024-01-01 00:00', "
        "timestamp '2024-01-02 00:00', interval '12 hours')"
    )
    assert rows == [
        (_dt.datetime(2024, 1, 1, 0, 0),),
        (_dt.datetime(2024, 1, 1, 12, 0),),
        (_dt.datetime(2024, 1, 2, 0, 0),),
    ]


def test_generate_series_timestamp_month_step_clamps():
    # month stepping is calendar-aware; day clamps to month length (Jan 31 -> Feb 29)
    rows = _rows(
        "SELECT generate_series(timestamp '2024-01-31', timestamp '2024-03-31', interval '1 month')"
    )
    assert rows == [
        (_dt.datetime(2024, 1, 31),),
        (_dt.datetime(2024, 2, 29),),
        (_dt.datetime(2024, 3, 29),),
    ]


def test_generate_series_timestamp_descending():
    rows = _rows(
        "SELECT generate_series(timestamp '2024-01-03', timestamp '2024-01-01', interval '-1 day')"
    )
    assert rows == [
        (_dt.datetime(2024, 1, 3),),
        (_dt.datetime(2024, 1, 2),),
        (_dt.datetime(2024, 1, 1),),
    ]


def test_generate_series_timestamp_empty_when_wrong_direction():
    # positive step but start > stop -> no rows (Postgres)
    rows = _rows(
        "SELECT generate_series(timestamp '2024-01-03', timestamp '2024-01-01', interval '1 day')"
    )
    assert rows == []


def test_generate_series_timestamp_column_type():
    r = _run(
        "SELECT * FROM generate_series(timestamp '2024-01-01', "
        "timestamp '2024-01-02', interval '1 day')"
    )
    assert [c.type_tag for c in r.columns] == ["timestamp"]


# --------------------------------------------------------------------------- #
# regexp_matches as a set-returning function (#152)
# --------------------------------------------------------------------------- #


def test_regexp_matches_global_one_row_per_match():
    assert _rows("SELECT * FROM regexp_matches('foobarbaz', 'ba.', 'g') AS m") == [
        (["bar"],),
        (["baz"],),
    ]


def test_regexp_matches_capture_groups():
    assert _rows("SELECT * FROM regexp_matches('a1b2', '([a-z])([0-9])', 'g') AS m") == [
        (["a", "1"],),
        (["b", "2"],),
    ]


def test_regexp_matches_no_global_first_match_only():
    assert _rows("SELECT regexp_matches('a1b2c3', '([a-z])([0-9])')") == [(["a", "1"],)]


def test_regexp_matches_select_list_form():
    assert _rows("SELECT regexp_matches('xxaxxbxx', 'x(a|b)x', 'g')") == [(["a"],), (["b"],)]


def test_regexp_matches_no_match_no_rows():
    assert _rows("SELECT * FROM regexp_matches('abc', 'z', 'g') AS m") == []


def test_regexp_matches_case_insensitive_flag():
    assert _rows("SELECT * FROM regexp_matches('AbaBA', 'a', 'gi') AS m") == [
        (["A"],),
        (["a"],),
        (["A"],),
    ]


def test_regexp_matches_column_type_is_text_array():
    r = _run("SELECT * FROM regexp_matches('ab', '(a)(b)') AS m")
    assert [c.type_tag for c in r.columns] == ["text[]"]


# --------------------------------------------------------------------------- #
# jsonb_each / jsonb_each_text record SRFs (#155)
# --------------------------------------------------------------------------- #


def test_jsonb_each_from():
    rows = _rows('SELECT * FROM jsonb_each(\'{"a":1,"b":"x"}\'::jsonb) ORDER BY key')
    assert rows == [("a", 1), ("b", "x")]


def test_jsonb_each_columns():
    r = _run("SELECT * FROM jsonb_each('{\"a\":1}'::jsonb)")
    assert [(c.name, c.type_tag) for c in r.columns] == [("key", "text"), ("value", "json")]


def test_jsonb_each_text_stringifies_values():
    rows = _rows(
        'SELECT * FROM jsonb_each_text(\'{"a":1,"b":"x","c":{"d":2}}\'::jsonb) ORDER BY key'
    )
    assert rows == [("a", "1"), ("b", "x"), ("c", '{"d": 2}')]


def test_jsonb_each_text_columns_are_text():
    r = _run("SELECT * FROM jsonb_each_text('{\"a\":1}'::jsonb)")
    assert [(c.name, c.type_tag) for c in r.columns] == [("key", "text"), ("value", "text")]


def test_jsonb_each_column_aliases():
    assert _cols("SELECT * FROM jsonb_each('{\"a\":1}'::jsonb) AS t(k, v)") == ["k", "v"]
    assert _rows("SELECT k, v FROM jsonb_each('{\"a\":1}'::jsonb) AS t(k, v)") == [("a", 1)]


def test_jsonb_each_where_and_order():
    rows = _rows(
        'SELECT key FROM jsonb_each(\'{"a":1,"b":2,"c":3}\'::jsonb) '
        "WHERE value::int > 1 ORDER BY key"
    )
    assert rows == [("b",), ("c",)]


def test_jsonb_each_with_ordinality():
    r = _run("SELECT * FROM jsonb_each('{\"x\":9}'::jsonb) WITH ORDINALITY")
    assert [c.name for c in r.columns] == ["key", "value", "ordinality"]
    assert r.rows == [("x", 9, 1)]


def test_jsonb_each_empty_object():
    assert _rows("SELECT * FROM jsonb_each('{}'::jsonb)") == []


def test_json_each_text_bool_and_null():
    rows = _rows('SELECT * FROM json_each_text(\'{"a":true,"b":null}\'::json) ORDER BY key')
    assert rows == [("a", "true"), ("b", None)]


def test_computed_projection_over_srf():
    assert _rows("select x * 2 from generate_series(1, 3) as t(x)") == [(2,), (4,), (6,)]
    assert _rows("select x * 2 as y from generate_series(1, 3) as t(x) order by y desc") == [
        (6,),
        (4,),
        (2,),
    ]
    assert _rows("select 1 from generate_series(1, 3)") == [(1,), (1,), (1,)]


def test_computed_projection_over_catalog_table():
    st = Storage(":memory:")
    try:
        sess = Session(database=DB)
        run_sql(st, DB, "create table pc (a int4)", session=sess)
        sql = "select upper(relname) from pg_class where relname = 'pc'"
        res = run_sql(st, DB, sql, session=sess)[-1]
        assert res.rows == [("PC",)]
        res = run_sql(st, DB, "select 1 from pg_namespace limit 1", session=sess)[-1]
        assert res.rows == [(1,)]
    finally:
        st.close()


def test_generate_series_accepts_untyped_text_bounds():
    """A bound arriving as text is parsed as a number, as Postgres does.

    An untyped parameter (`generate_series(1, $1)` with `$1` sent without a
    type OID) reaches the SRF as a string — nothing upstream coerced it,
    because the wire gave no type. Postgres infers the parameter's type from
    the argument position and parses it as an integer.

    This is not academic: pgx's `ensureConnValid` helper runs exactly that
    query and is called at the end of 66 `pgconn` tests, so rejecting it took
    otherwise-passing tests down with it. Fixing it moved that package from 86
    failures to 29.
    """
    assert _rows("select generate_series(1, '3')") == [(1,), (2,), (3,)]
    assert _rows("select generate_series('1', '3')") == [(1,), (2,), (3,)]
    # A numeric step still works alongside coerced bounds.
    assert _rows("select generate_series('1', '10', 3)") == [(1,), (4,), (7,), (10,)]

    # NOT covered, deliberately: a QUOTED third argument
    # (`generate_series(1, 10, '3')`). sqlglot parses that into an `Interval`
    # node at parse time, so it arrives as an interval and never reaches this
    # coercion — a separate parser-level quirk, not this fix's job. Recorded
    # here rather than silently omitted.


def test_generate_series_still_rejects_non_numeric_bounds():
    """Coercion is narrow: only strings that parse cleanly become numbers.

    Guards the over-permissive direction — a bound that is genuinely not a
    number must still raise, not silently yield an empty or nonsense series.
    """
    import pytest as _pytest

    from secantus.sql import errors

    with _pytest.raises(errors.SQLError) as exc:
        _rows("select generate_series('a', 'b')")
    assert "integer / numeric" in str(exc.value)
