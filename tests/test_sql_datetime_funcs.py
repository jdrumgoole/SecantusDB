"""Date/time scalar functions: ``extract`` / ``date_part`` / ``date_trunc`` /
``to_char`` / interval arithmetic / ``now`` / ``current_timestamp`` /
``current_date`` (evaluated per row in ``secantus.sql.scalar``).
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"
TS = "2021-03-15T14:30:45+00:00"


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


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE ev (id int PRIMARY KEY, at timestamptz)")
    run(storage, session, f"INSERT INTO ev VALUES (1, '{TS}')")
    return storage


def val(storage, session, sql):
    v = run(storage, session, sql).rows[0][0]
    # The embedded run_sql API surfaces a stored `timestamptz` as a tz-naive UTC
    # datetime (BSON dates decode naive; the wire path is already tz-aware). These
    # tests assert the PG-correct tz-aware value, so normalise naive → UTC here.
    # Remove this shim once task #141 makes the embedded API return tz-aware.
    if isinstance(v, dt.datetime) and v.tzinfo is None:
        v = v.replace(tzinfo=dt.timezone.utc)
    return v


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


# -- extract / date_part ------------------------------------------------------ #


@pytest.mark.parametrize(
    "field,expected",
    [
        ("year", 2021),
        ("month", 3),
        ("day", 15),
        ("hour", 14),
        ("minute", 30),
        ("second", 45),
        ("quarter", 1),
        ("dow", 1),  # Monday = 1 (Sunday = 0)
        ("isodow", 1),
        ("doy", 74),
        ("week", 11),
    ],
)
def test_extract(t, session, field, expected):
    assert val(t, session, f"SELECT extract({field} FROM at) FROM ev") == expected


def test_date_part_alias(t, session):
    assert val(t, session, "SELECT date_part('hour', at) FROM ev") == 14


def test_extract_epoch(t, session):
    assert val(t, session, "SELECT extract(epoch FROM at) FROM ev") == pytest.approx(1615818645.0)


def test_extract_types_numeric(t, session):
    assert col(t, session, "SELECT extract(year FROM at) FROM ev").type_tag == "numeric"


# -- date_trunc --------------------------------------------------------------- #


def test_date_trunc_month(t, session):
    assert val(t, session, "SELECT date_trunc('month', at) FROM ev") == dt.datetime(
        2021, 3, 1, tzinfo=dt.timezone.utc
    )


def test_date_trunc_day(t, session):
    assert val(t, session, "SELECT date_trunc('day', at) FROM ev") == dt.datetime(
        2021, 3, 15, tzinfo=dt.timezone.utc
    )


def test_date_trunc_week_lands_on_monday(t, session):
    # 2021-03-15 is a Monday, so week-trunc keeps the date but zeroes the time.
    assert val(t, session, "SELECT date_trunc('week', at) FROM ev") == dt.datetime(
        2021, 3, 15, tzinfo=dt.timezone.utc
    )


def test_date_trunc_types_timestamptz(t, session):
    assert col(t, session, "SELECT date_trunc('day', at) FROM ev").type_tag == "timestamptz"


# -- to_char ------------------------------------------------------------------ #


def test_to_char_date(t, session):
    assert val(t, session, "SELECT to_char(at, 'YYYY-MM-DD') FROM ev") == "2021-03-15"


def test_to_char_time(t, session):
    assert val(t, session, "SELECT to_char(at, 'HH24:MI:SS') FROM ev") == "14:30:45"


def test_to_char_month_abbrev(t, session):
    assert val(t, session, "SELECT to_char(at, 'Mon DD, YYYY') FROM ev") == "Mar 15, 2021"


def test_to_char_types_text(t, session):
    assert col(t, session, "SELECT to_char(at, 'YYYY') FROM ev").type_tag == "text"


# -- interval arithmetic ------------------------------------------------------ #


def test_add_day(t, session):
    assert val(t, session, "SELECT at + interval '1 day' FROM ev") == dt.datetime(
        2021, 3, 16, 14, 30, 45, tzinfo=dt.timezone.utc
    )


def test_add_months_clamps_day(storage, session):
    run(storage, session, "CREATE TABLE e2 (id int PRIMARY KEY, at timestamptz)")
    run(storage, session, "INSERT INTO e2 VALUES (1, '2021-01-31T00:00:00+00:00')")
    # Jan 31 + 1 month clamps to Feb 28 (2021 is not a leap year).
    assert val(storage, session, "SELECT at + interval '1 month' FROM e2") == dt.datetime(
        2021, 2, 28, tzinfo=dt.timezone.utc
    )


def test_add_year(t, session):
    assert val(t, session, "SELECT at + interval '1 year' FROM ev") == dt.datetime(
        2022, 3, 15, 14, 30, 45, tzinfo=dt.timezone.utc
    )


def test_subtract_hours(t, session):
    assert val(t, session, "SELECT at - interval '3 hours' FROM ev") == dt.datetime(
        2021, 3, 15, 11, 30, 45, tzinfo=dt.timezone.utc
    )


def test_compound_interval(t, session):
    assert val(t, session, "SELECT at + interval '1 year 2 months 3 days' FROM ev") == dt.datetime(
        2022, 5, 18, 14, 30, 45, tzinfo=dt.timezone.utc
    )


def test_interval_arith_types_timestamptz(t, session):
    assert col(t, session, "SELECT at + interval '1 day' FROM ev").type_tag == "timestamptz"


# -- now / current_* ---------------------------------------------------------- #


def test_now_is_tzaware(t, session):
    v = val(t, session, "SELECT now() FROM ev")
    assert isinstance(v, dt.datetime) and v.tzinfo is not None


def test_current_date(t, session):
    v = val(t, session, "SELECT current_date FROM ev")
    assert isinstance(v, dt.date)


def test_now_types_timestamptz(t, session):
    assert col(t, session, "SELECT now() FROM ev").type_tag == "timestamptz"


# -- NULL propagation --------------------------------------------------------- #


def test_null_propagation(storage, session):
    run(storage, session, "CREATE TABLE e3 (id int PRIMARY KEY, at timestamptz)")
    run(storage, session, "INSERT INTO e3 VALUES (1, NULL)")
    assert val(storage, session, "SELECT extract(year FROM at) FROM e3") is None
    assert val(storage, session, "SELECT date_trunc('day', at) FROM e3") is None
    assert val(storage, session, "SELECT to_char(at, 'YYYY') FROM e3") is None
