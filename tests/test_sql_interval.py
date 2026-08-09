"""Interval type + functions (#110): interval literals, interval / date
arithmetic, make_interval / justify_* / age, and extract(field from interval).
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql import intervals, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure intervals.py
# --------------------------------------------------------------------------- #


def test_parse_and_render_roundtrip():
    assert intervals.render(intervals.parse("1 year 2 months 3 days")) == "1 year 2 mons 3 days"
    assert (
        intervals.render(intervals.parse("1 day 2 hours 3 minutes 4 seconds")) == "1 day 02:03:04"
    )
    assert intervals.render(intervals.parse("90 minutes")) == "01:30:00"
    assert intervals.render(intervals.parse("04:05:06")) == "04:05:06"
    assert intervals.render(intervals.parse("1 day ago")) == "-1 day"
    assert intervals.render(intervals.parse("0")) == "00:00:00"


def test_from_unit_fractional_month():
    assert intervals.render(intervals.from_unit(1.5, "month")) == "1 mon 15 days"


def test_add_sub_neg_mul():
    a, b = intervals.parse("1 day"), intervals.parse("2 hours")
    assert intervals.render(intervals.add(a, b)) == "1 day 02:00:00"
    assert intervals.render(intervals.sub(a, b)) == "1 day -02:00:00"
    assert intervals.render(intervals.neg(intervals.parse("1 day 1 hour"))) == "-1 day -01:00:00"
    assert intervals.render(intervals.mul(intervals.parse("1 hour"), 2.5)) == "02:30:00"


def test_justify():
    assert (
        intervals.render(intervals.justify_hours(intervals.parse("25 hours"))) == "1 day 01:00:00"
    )
    assert intervals.render(intervals.justify_days(intervals.parse("35 days"))) == "1 mon 5 days"
    assert (
        intervals.render(intervals.justify_interval(intervals.parse("1 mon 33 days 25 hours")))
        == "2 mons 4 days 01:00:00"
    )


def test_to_date_clamps_month_end():
    assert intervals.to_date(dt.date(2020, 1, 31), intervals.parse("1 month"), 1) == dt.date(
        2020, 2, 29
    )


def test_age_and_diff():
    assert intervals.render(intervals.age(dt.date(2021, 3, 15), dt.date(2020, 1, 20))) == (
        "1 year 1 mon 23 days"
    )
    assert (
        intervals.render(intervals.diff(dt.datetime(2020, 1, 2, 12), dt.datetime(2020, 1, 1, 10)))
        == "1 day 02:00:00"
    )


def test_extract_field():
    assert intervals.extract_field("year", intervals.parse("2 years 3 months")) == 2
    assert intervals.extract_field("month", intervals.parse("2 years 3 months")) == 3
    assert intervals.extract_field("second", intervals.parse("00:01:30.5")) == 30.5


def test_bad_literal_raises():
    with pytest.raises(intervals.IntervalError):
        intervals.parse("5 fortnights")


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


def rendered(storage, session, sql):
    """The wire-text rendering of a single interval result."""
    r = run(storage, session, sql)
    from secantus.sql import typemap

    return typemap.to_pg_text(r.rows[0][0], r.columns[0].type_tag).decode()


def test_interval_literal_typed(storage, session):
    assert col(storage, session, "SELECT interval '1 day'").type_tag == "interval"


def test_interval_literal_value(storage, session):
    assert val(storage, session, "SELECT interval '1 day'") == intervals.parse("1 day")


def test_interval_renders_pg_text(storage, session):
    assert rendered(storage, session, "SELECT interval '1 year 2 months 3 days'") == (
        "1 year 2 mons 3 days"
    )
    assert rendered(storage, session, "SELECT interval '90 minutes'") == "01:30:00"


def test_interval_arithmetic(storage, session):
    assert rendered(storage, session, "SELECT interval '1 day' + interval '2 hours'") == (
        "1 day 02:00:00"
    )
    assert rendered(storage, session, "SELECT interval '1 hour' * 3") == "03:00:00"
    assert rendered(storage, session, "SELECT - interval '1 day'") == "-1 day"


def test_interval_arithmetic_typed(storage, session):
    assert col(storage, session, "SELECT interval '1 day' + interval '2 hours'").type_tag == (
        "interval"
    )


def test_timestamp_plus_interval(storage, session):
    c = col(storage, session, "SELECT timestamp '2020-01-31' + interval '1 month'")
    # ``timestamp + interval`` -> naive ``timestamp`` (without time zone), per PG (#143).
    assert c.type_tag == "timestamp"
    v = val(storage, session, "SELECT timestamp '2020-01-31' + interval '1 month'")
    assert v == dt.datetime(2020, 2, 29)
    assert v.tzinfo is None


def test_timestamp_minus_timestamp_is_interval(storage, session):
    c = col(storage, session, "SELECT timestamp '2020-03-15' - timestamp '2020-01-01'")
    assert c.type_tag == "interval"
    assert rendered(storage, session, "SELECT timestamp '2020-03-15' - timestamp '2020-01-01'") == (
        "74 days"
    )


def test_make_interval(storage, session):
    assert col(storage, session, "SELECT make_interval(1, 2, 0, 3, 4, 5, 6)").type_tag == "interval"
    assert rendered(storage, session, "SELECT make_interval(1, 2, 0, 3, 4, 5, 6)") == (
        "1 year 2 mons 3 days 04:05:06"
    )


def test_justify_functions(storage, session):
    assert rendered(storage, session, "SELECT justify_hours(interval '25 hours')") == (
        "1 day 01:00:00"
    )
    assert rendered(storage, session, "SELECT justify_days(interval '35 days')") == "1 mon 5 days"


def test_age(storage, session):
    assert (
        col(storage, session, "SELECT age(timestamp '2021-03-15', timestamp '2020-01-20')").type_tag
        == "interval"
    )
    assert (
        rendered(storage, session, "SELECT age(timestamp '2021-03-15', timestamp '2020-01-20')")
        == "1 year 1 mon 23 days"
    )


def test_extract_from_interval(storage, session):
    assert val(storage, session, "SELECT extract(day from interval '3 days 4 hours')") == 3
    assert val(storage, session, "SELECT extract(hour from interval '3 days 4 hours')") == 4


def test_interval_cast(storage, session):
    assert val(storage, session, "SELECT '2 days'::interval") == intervals.parse("2 days")


@pytest.fixture
def durations(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, dur interval)")
    run(storage, session, "INSERT INTO t VALUES (1, interval '2 hours 30 minutes')")
    run(storage, session, "INSERT INTO t VALUES (2, interval '1 day')")
    return storage


def test_interval_column_roundtrip(durations, session):
    assert val(durations, session, "SELECT dur FROM t WHERE id = 1") == intervals.parse(
        "2 hours 30 minutes"
    )
    assert rendered(durations, session, "SELECT dur FROM t WHERE id = 1") == "02:30:00"


def test_interval_column_arithmetic(durations, session):
    assert rendered(durations, session, "SELECT dur + interval '1 hour' FROM t WHERE id = 1") == (
        "03:30:00"
    )


# --------------------------------------------------------------------------- #
# time ± interval (crashed with a TypeError: the overload was missing entirely)
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    ("sql", "expected"),
    [
        ("SELECT time '10:00:00' + interval '2 hours'", "12:00:00"),
        ("SELECT interval '2 hours' + time '10:00:00'", "12:00:00"),
        ("SELECT time '00:30:00' - interval '2 hours'", "22:30:00"),
        # Postgres wraps into a single day rather than carrying into the next.
        ("SELECT time '23:00:00' + interval '3 hours'", "02:00:00"),
        # ``months`` / ``days`` are dropped — a time of day has no date to carry.
        ("SELECT time '10:00:00' + interval '5 days 1 hour'", "11:00:00"),
        # Trailing zeros are stripped, matching the canonical ``time`` form.
        ("SELECT time '10:00:00.5' + interval '1 second'", "10:00:01.5"),
    ],
)
def test_time_plus_interval(storage, session, sql, expected):
    assert val(storage, session, sql) == expected


def test_interval_times_unknown_text_operand(storage, session):
    """An unknown-type text operand beside an interval resolves numerically, the
    way Postgres coerces an unknown literal — pgjdbc binds parameters typeless,
    so ``$1 * $2::interval`` reaches the evaluator as ``str * interval``."""
    assert val(storage, session, "SELECT '3' * interval '1 day'") == intervals.parse("3 days")
    assert val(storage, session, "SELECT interval '1 day' * '3'") == intervals.parse("3 days")
    assert val(storage, session, "SELECT interval '1 day' / '2'") == intervals.parse("12 hours")


def test_interval_arithmetic_in_where_is_not_pushed_down(storage, session):
    """``ts < ts + n * interval`` has no aggregation-expression form: the Mongo
    pushdown drops casts, so it would hand ``$multiply`` the raw interval text.
    The predicate must fall back to per-row scalar evaluation instead."""
    run(storage, session, "CREATE TABLE ev (id int PRIMARY KEY, at timestamptz)")
    run(storage, session, "INSERT INTO ev VALUES (1, '2020-01-05 00:00:00+00')")
    run(storage, session, "INSERT INTO ev VALUES (2, '2020-01-25 00:00:00+00')")
    r = run(
        storage,
        session,
        "SELECT id FROM ev WHERE at < ('2020-01-01 00:00:00+00'::timestamptz "
        "+ '10' * '1 day'::interval) ORDER BY id",
    )
    assert [row[0] for row in r.rows] == [1]


def test_leap_second_time_carries_forward(storage, session):
    """``'23:59:60'::time`` is ``24:00:00`` in Postgres — the top of the time
    domain, one microsecond past what a Python ``time`` can hold. Storing the
    literal ``60`` instead left a value nothing could parse, so *any* arithmetic
    on it (``time - time`` as well as ``time ± interval``) died with a bare
    ValueError and reached the client as ``internal error``.
    """
    assert val(storage, session, "SELECT time '23:59:60'") == "24:00:00"
    assert val(storage, session, "SELECT time '10:00:60'") == "10:01:00"
    assert val(storage, session, "SELECT time '23:59:60' - time '00:00:00'") == intervals.parse(
        "24 hours"
    )
    assert val(storage, session, "SELECT time '23:59:60' + interval '1 second'") == "00:00:01"


def test_timetz_plus_interval_keeps_the_offset(storage, session):
    """``timetz ± interval`` shifts the time of day and carries the zone offset
    through untouched, wrapping within the day like plain ``time``. There was no
    overload at all, so pgjdbc's ``?::time with time zone + ?`` crashed."""
    q = "SELECT time with time zone '{}' {} interval '{}'"
    assert val(storage, session, q.format("01:02:03+00", "+", "1 hour")) == "02:02:03+00:00"
    assert val(storage, session, q.format("23:30:00+02", "+", "1 hour")) == "00:30:00+02:00"
    assert val(storage, session, q.format("01:02:03+00", "-", "2 hours")) == "23:02:03+00:00"
    assert (
        val(storage, session, "SELECT interval '1 hour' + time with time zone '01:02:03+00'")
        == "02:02:03+00:00"
    )


def test_date_column_compared_against_computed_timestamp(storage, session):
    """A stored ``date`` is ISO text; comparing it against a computed
    ``timestamp`` (``ts + n * interval``) raised TypeError once the predicate
    correctly stopped being pushed down. Postgres promotes the date to midnight.
    This is pgjdbc's ``IntervalTest.stringToIntervalCoercion`` verbatim.
    """
    run(storage, session, "CREATE TABLE testdate (v date)")
    for d in ("2010-01-01", "2010-01-02", "2010-01-04", "2010-01-05"):
        run(storage, session, f"INSERT INTO testdate VALUES ('{d}')")
    r = run(
        storage,
        session,
        "SELECT v FROM testdate WHERE v < ('2010-01-01'::timestamp with time zone "
        "+ 2 * '1 day'::interval) ORDER BY v",
    )
    assert [row[0] for row in r.rows] == ["2010-01-01", "2010-01-02"]
