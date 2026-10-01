"""PostgreSQL DecodeDateTime conformance for date / timestamp / timestamptz input.

Every expectation below was produced by PostgreSQL 15 (``SET timezone =
'UTC'``, DateStyle ``ISO, MDY``) -- ``SELECT '<input>'::<type>::text``.
Runs against the real WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql, typemap
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


@pytest.fixture
def env(tmp_path):
    st = Storage(str(tmp_path))
    sess = Session(database=DB)
    typemap.set_render_session(sess)
    try:
        run_sql(st, DB, "SET TimeZone = 'UTC'", session=sess)
        yield st, sess
    finally:
        typemap.set_render_session(None)
        st.close()


def q(env, sql):
    st, sess = env
    return run_sql(st, DB, sql, session=sess)[-1].rows


VALUES = [
    ("2020-1-5", "date", "2020-01-05"),
    ("20200105", "timestamp", "2020-01-05 00:00:00"),
    ("Jan 5, 2020", "date", "2020-01-05"),
    ("5 January 2020", "timestamp", "2020-01-05 00:00:00"),
    ("5-Jan-2020", "date", "2020-01-05"),
    ("Jan-05-2020", "timestamptz", "2020-01-05 00:00:00+00"),
    ("2020-Jan-05", "date", "2020-01-05"),
    ("1/5/2020", "date", "2020-01-05"),
    ("01/05/20", "timestamp", "2020-01-05 00:00:00"),
    ("1/5/70", "date", "1970-01-05"),
    ("1/5/69", "date", "2069-01-05"),
    ("2020.005", "date", "2020-01-05"),
    ("J2458854", "timestamp", "2020-01-05 00:00:00"),
    ("y2020m01d05", "date", "2020-01-05"),
    ("Mon Jan 5 2020", "timestamp", "2020-01-05 00:00:00"),
    ("Jan 5 2020 BC", "date", "2020-01-05 BC"),
    ("Jan 5 2020 BC", "timestamptz", "2020-01-05 00:00:00+00 BC"),
    ("05.01.2020", "date", "2020-05-01"),
    ("2020/01/05", "timestamp", "2020-01-05 00:00:00"),
    ("Feb 29 2020", "timestamp", "2020-02-29 00:00:00"),
    ("epoch", "timestamptz", "1970-01-01 00:00:00+00"),
    ("20200105T103000", "timestamp", "2020-01-05 10:30:00"),
    ("Jan 5 2020 10:30 PM", "timestamp", "2020-01-05 22:30:00"),
    ("Jan 5 2020 12:00 AM", "timestamp", "2020-01-05 00:00:00"),
    ("2020-01-05 24:00:00", "timestamp", "2020-01-06 00:00:00"),
    ("2020-01-05 23:59:60", "timestamp", "2020-01-06 00:00:00"),
    ("2020-01-05 10:30:00.1234567", "timestamp", "2020-01-05 10:30:00.123457"),
    ("99999-01-01", "timestamptz", "99999-01-01 00:00:00+00"),
    ("2020-01-05 10:30 EST", "timestamptz", "2020-01-05 15:30:00+00"),
    ("2020-01-05 10:30 PST", "timestamp", "2020-01-05 10:30:00"),
    ("2020-07-05 10:30:00 America/New_York", "timestamptz", "2020-07-05 14:30:00+00"),
    ("2020-01-05 10:30+05:30", "timestamptz", "2020-01-05 05:00:00+00"),
    ("2020-01-05 10:30 -0800", "timestamptz", "2020-01-05 18:30:00+00"),
    ("Jan 5 10:30 2020", "timestamp", "2020-01-05 10:30:00"),
    ("2020-01-05T10:30:00Z", "timestamptz", "2020-01-05 10:30:00+00"),
    ("January 5, 2020 at 10:30", "timestamp", "2020-01-05 10:30:00"),
    ("Sat Jan 05 10:30:00 2020 CET", "timestamptz", "2020-01-05 09:30:00+00"),
    ("2020 Jan 5", "date", "2020-01-05"),
    ("5 Jan 20", "date", "2020-01-05"),
    ("1999-01-08 04:05:06 -8:00", "timestamptz", "1999-01-08 12:05:06+00"),
]


@pytest.mark.parametrize(("text", "tag", "expected"), VALUES)
def test_input_matches_postgres(env, text, tag, expected):
    assert q(env, f"SELECT '{text}'::{tag}::text") == [(expected,)]


ERRORS = [
    ("Jan 32 2020", "date", "22008", 'date/time field value out of range: "Jan 32 2020"'),
    ("Feb 29 2021", "timestamp", "22008", 'date/time field value out of range: "Feb 29 2021"'),
    ("13/01/2020", "date", "22008", 'date/time field value out of range: "13/01/2020"'),
    ("2020-13-01", "timestamp", "22008", 'date/time field value out of range: "2020-13-01"'),
    ("", "timestamptz", "22007", 'invalid input syntax for type timestamp with time zone: ""'),
    ("garbage", "date", "22007", 'invalid input syntax for type date: "garbage"'),
    ("Jan 2020", "timestamp", "22007", 'invalid input syntax for type timestamp: "Jan 2020"'),
    (
        "Jan 5 2020 13:00 PM",
        "timestamp",
        "22008",
        'date/time field value out of range: "Jan 5 2020 13:00 PM"',
    ),
    (
        "allballs",
        "timestamptz",
        "22007",
        'invalid input syntax for type timestamp with time zone: "allballs"',
    ),
    (
        "2020-01-05 10:30:61",
        "date",
        "22008",
        'date/time field value out of range: "2020-01-05 10:30:61"',
    ),
    (
        "2020-01-05 10:30 nosuchzone",
        "timestamptz",
        "22007",
        'invalid input syntax for type timestamp with time zone: "2020-01-05 10:30 nosuchzone"',
    ),
    (
        "2020-01-05 10:30 +25",
        "timestamptz",
        "22009",
        'time zone displacement out of range: "2020-01-05 10:30 +25"',
    ),
    ("294277-01-01", "timestamp", "22008", 'timestamp out of range: "294277-01-01"'),
    ("4714-11-23 BC", "date", "22008", 'date out of range: "4714-11-23 BC"'),
    ("0000-01-01", "date", "22008", 'date/time field value out of range: "0000-01-01"'),
]


@pytest.mark.parametrize(("text", "tag", "sqlstate", "message"), ERRORS)
def test_input_errors_match_postgres(env, text, tag, sqlstate, message):
    with pytest.raises(errors.SQLError) as exc:
        q(env, f"SELECT '{text}'::{tag}::text")
    assert exc.value.sqlstate == sqlstate
    assert exc.value.message == message


def test_dmy_and_ymd_datestyle(env):
    q(env, "SET DateStyle = 'ISO, DMY'")
    assert q(env, "SELECT '1/5/2020'::date::text") == [("2020-05-01",)]
    assert q(env, "SELECT '05/01/20'::date::text") == [("2020-01-05",)]
    q(env, "SET DateStyle = 'ISO, YMD'")
    assert q(env, "SELECT '20/01/05'::date::text") == [("2020-01-05",)]


def test_month_day_overflow_carries_datestyle_hint(env):
    with pytest.raises(errors.SQLError) as exc:
        q(env, "SELECT '13/01/2020'::date")
    assert exc.value.diag.get("H") == 'Perhaps you need a different "datestyle" setting.'


def test_session_zone_dst_gap_and_overlap(env):
    # Overlap reads as the later (standard) offset, a gap as the earlier one.
    q(env, "SET TimeZone = 'America/New_York'")
    assert q(env, "SELECT '2020-11-01 01:30'::timestamptz::text") == [("2020-11-01 01:30:00-05",)]
    assert q(env, "SELECT '2020-03-08 02:30'::timestamptz::text") == [("2020-03-08 03:30:00-04",)]


def test_at_time_zone_offsets_and_intervals(env):
    base = "'2020-01-05 10:30+00'::timestamptz"
    assert q(env, f"SELECT ({base} at time zone '+05')::text") == [("2020-01-05 05:30:00",)]
    assert q(env, f"SELECT ({base} at time zone '-03:30')::text") == [("2020-01-05 14:00:00",)]
    assert q(env, f"SELECT ({base} at time zone 'EST')::text") == [("2020-01-05 05:30:00",)]
    assert q(env, f"SELECT ({base} at time zone interval '+05:00')::text") == [
        ("2020-01-05 15:30:00",)
    ]
    assert q(env, "SELECT timezone('UTC', '2020-01-05 10:30'::timestamp)::text") == [
        ("2020-01-05 10:30:00+00",)
    ]
