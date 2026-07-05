"""Interval type + functions (#110): interval literals, interval / date
arithmetic, make_interval / justify_* / age, and extract(field from interval).
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql import intervals, run_sql
from secantus.sql.session import Session
from sqlfake import FakeStorage

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
def storage():
    return FakeStorage()


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
    assert c.type_tag == "timestamptz"
    assert val(storage, session, "SELECT timestamp '2020-01-31' + interval '1 month'") == (
        dt.datetime(2020, 2, 29)
    )


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
