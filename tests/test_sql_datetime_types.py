"""date / time / timetz distinct types (#113): literals, casts, current_date /
current_time, date-date / date+int / date+interval / time-time arithmetic, and
column round-trips.
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql import datetimes, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure datetimes.py
# --------------------------------------------------------------------------- #


def test_parse_date():
    assert datetimes.parse_date("2020-01-15") == "2020-01-15"
    assert datetimes.parse_date(dt.date(2020, 1, 15)) == "2020-01-15"
    assert datetimes.parse_date(dt.datetime(2020, 1, 15, 10, 30)) == "2020-01-15"
    with pytest.raises(datetimes.DateTimeError):
        datetimes.parse_date("nope")


def test_parse_time():
    assert datetimes.parse_time("14:30") == "14:30:00"
    assert datetimes.parse_time("9:5:3") == "09:05:03"
    assert datetimes.parse_time("14:30:00.500000") == "14:30:00.5"
    with pytest.raises(datetimes.DateTimeError):
        datetimes.parse_time("nope")


def test_parse_timetz():
    assert datetimes.parse_timetz("14:30") == "14:30:00+00:00"
    assert datetimes.parse_timetz("14:30:00+02") == "14:30:00+02:00"
    assert datetimes.parse_timetz("14:30:00-0530") == "14:30:00-05:30"


def test_parse_timetz_sub_minute_offset():
    # A zone offset can carry seconds (historical LMT zones); it round-trips.
    assert datetimes.parse_timetz("00:00:00+01:01:03") == "00:00:00+01:01:03"
    assert datetimes.render_timetz("00:00:00+01:01:03") == "00:00:00+01:01:03"
    # Whole-minute and whole-hour offsets still drop their trailing :00 groups.
    assert datetimes.render_timetz("00:00:00+01:00") == "00:00:00+01"
    assert datetimes.render_timetz("00:00:00+05:30") == "00:00:00+05:30"
    assert datetimes.split_timetz("00:00:00+01:01:03") == ("00:00:00", "+01:01:03")


def test_is_date_value():
    assert datetimes.is_date_value("2020-01-15") is True
    assert datetimes.is_date_value(dt.date(2020, 1, 1)) is True
    assert datetimes.is_date_value(dt.datetime(2020, 1, 1)) is False  # a timestamp, not a date
    assert datetimes.is_date_value("foo") is False


def test_date_arithmetic_helpers():
    assert datetimes.date_sub_date("2020-03-15", "2020-01-01") == 74
    assert datetimes.date_add_days("2020-01-31", 1) == "2020-02-01"
    assert datetimes.date_add_days("2020-03-01", -1) == "2020-02-29"


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


def test_date_literal_typed(storage, session):
    assert col(storage, session, "SELECT date '2020-01-15'").type_tag == "date"


def test_time_literal_typed(storage, session):
    assert col(storage, session, "SELECT time '14:30:00'").type_tag == "time"


def test_date_cast(storage, session):
    assert val(storage, session, "SELECT '2020-01-15'::date") == "2020-01-15"
    assert col(storage, session, "SELECT '2020-01-15'::date").type_tag == "date"


def test_time_cast(storage, session):
    assert val(storage, session, "SELECT '14:30'::time") == "14:30:00"


def test_timetz_cast(storage, session):
    assert val(storage, session, "SELECT '14:30:00+02'::timetz") == "14:30:00+02:00"
    assert col(storage, session, "SELECT '14:30:00+02'::timetz").type_tag == "timetz"


def test_current_date_typed_date(storage, session):
    c = col(storage, session, "SELECT current_date")
    assert c.type_tag == "date"
    assert isinstance(val(storage, session, "SELECT current_date"), dt.date)


def test_current_time_typed_timetz(storage, session):
    assert col(storage, session, "SELECT current_time").type_tag == "timetz"


def test_date_minus_date_is_int(storage, session):
    c = col(storage, session, "SELECT date '2020-03-15' - date '2020-01-01'")
    assert c.type_tag == "int4"
    assert val(storage, session, "SELECT date '2020-03-15' - date '2020-01-01'") == 74


def test_date_plus_int_is_date(storage, session):
    c = col(storage, session, "SELECT date '2020-01-31' + 1")
    assert c.type_tag == "date"
    assert val(storage, session, "SELECT date '2020-01-31' + 1") == "2020-02-01"


def test_date_plus_interval_is_timestamp(storage, session):
    c = col(storage, session, "SELECT date '2020-01-15' + interval '1 day 2 hours'")
    # ``date + interval`` -> naive ``timestamp`` (without time zone), per Postgres (#143).
    assert c.type_tag == "timestamp"
    assert val(storage, session, "SELECT date '2020-01-15' + interval '1 day 2 hours'") == (
        dt.datetime(2020, 1, 16, 2, 0)
    )
    assert (
        val(storage, session, "SELECT date '2020-01-15' + interval '1 day 2 hours'").tzinfo is None
    )


def test_time_minus_time_is_interval(storage, session):
    from secantus.sql import intervals

    c = col(storage, session, "SELECT time '15:00' - time '13:30'")
    assert c.type_tag == "interval"
    assert val(storage, session, "SELECT time '15:00' - time '13:30'") == intervals.parse(
        "1 hour 30 minutes"
    )


@pytest.fixture
def events(storage, session):
    run(storage, session, "CREATE TABLE ev (id int PRIMARY KEY, d date, t time, ttz timetz)")
    run(storage, session, "INSERT INTO ev VALUES (1, '2020-06-15', '09:00', '09:00:00+02')")
    run(storage, session, "INSERT INTO ev VALUES (2, '2019-12-25', '23:30', '23:30:00-05')")
    return storage


def test_date_column_roundtrip(events, session):
    assert val(events, session, "SELECT d FROM ev WHERE id = 1") == "2020-06-15"
    assert val(events, session, "SELECT t FROM ev WHERE id = 1") == "09:00:00"
    assert val(events, session, "SELECT ttz FROM ev WHERE id = 1") == "09:00:00+02:00"


def test_date_column_typed(events, session):
    row = run(events, session, "SELECT d, t, ttz FROM ev WHERE id = 1")
    assert [c.type_tag for c in row.columns] == ["date", "time", "timetz"]


def test_date_where_equality(events, session):
    assert val(events, session, "SELECT id FROM ev WHERE d = '2020-06-15'") == 1


def test_date_order_by(events, session):
    ids = [r[0] for r in run(events, session, "SELECT id FROM ev ORDER BY d").rows]
    assert ids == [2, 1]  # 2019-12-25 before 2020-06-15


# --------------------------------------------------------------------------- #
# #141: embedded run_sql returns tz-aware UTC for stored timestamptz columns
# --------------------------------------------------------------------------- #


def test_stored_timestamptz_is_tzaware(storage, session):
    run(storage, session, "CREATE TABLE tsz (id int PRIMARY KEY, at timestamptz)")
    run(storage, session, "INSERT INTO tsz VALUES (1, '2020-01-02T03:04:05+00:00')")
    v = val(storage, session, "SELECT at FROM tsz WHERE id = 1")
    assert v == dt.datetime(2020, 1, 2, 3, 4, 5, tzinfo=dt.timezone.utc)
    assert v.tzinfo is not None  # tz-aware, matching the wire render (not naive)


def test_timestamptz_array_elements_tzaware(storage, session):
    run(storage, session, "CREATE TABLE tsa (id int PRIMARY KEY, ats timestamptz[])")
    run(
        storage,
        session,
        "INSERT INTO tsa VALUES (1, ARRAY['2020-01-02T03:04:05+00:00'::timestamptz, "
        "'2021-06-07T08:09:10+00:00'::timestamptz])",
    )
    v = val(storage, session, "SELECT ats FROM tsa WHERE id = 1")
    assert list(v) == [
        dt.datetime(2020, 1, 2, 3, 4, 5, tzinfo=dt.timezone.utc),
        dt.datetime(2021, 6, 7, 8, 9, 10, tzinfo=dt.timezone.utc),
    ]
    assert all(x.tzinfo is not None for x in v)


def test_timestamptz_where_equality_matches_offset_literal(storage, session):
    # #142: WHERE ts = '...+00:00' / ::timestamptz used to match nothing because
    # the equality path didn't bridge the tz-aware literal against the tz-naive
    # stored value. Now it matches by instant.
    run(storage, session, "CREATE TABLE ev2 (id int PRIMARY KEY, at timestamptz)")
    run(storage, session, "INSERT INTO ev2 VALUES (1, '2020-01-02T03:04:05+00:00')")
    run(storage, session, "INSERT INTO ev2 VALUES (2, '2021-06-07T08:09:10+00:00')")
    for pred in (
        "at = '2020-01-02T03:04:05+00:00'",
        "at = '2020-01-02T03:04:05+00:00'::timestamptz",
        "at = timestamptz '2020-01-02T03:04:05+00:00'",
        "at = '2020-01-02T05:04:05+02:00'",  # same instant, different offset
    ):
        assert run(storage, session, f"SELECT id FROM ev2 WHERE {pred}").rows == [(1,)], pred
    # A different instant must not match.
    assert (
        run(storage, session, "SELECT id FROM ev2 WHERE at = '2020-01-02T03:04:05+02:00'").rows
        == []
    )
    # IN and <> also bridge the boundary.
    assert run(
        storage,
        session,
        "SELECT id FROM ev2 WHERE at IN ('2020-01-02T03:04:05+00:00', "
        "'2021-06-07T08:09:10+00:00') ORDER BY id",
    ).rows == [(1,), (2,)]
    assert run(
        storage, session, "SELECT id FROM ev2 WHERE at <> '2020-01-02T03:04:05+00:00'"
    ).rows == [(2,)]


def test_timestamptz_where_equality_uses_index(storage, session):
    # The indexed equality path (sortkey-encoded) must also bridge naive/aware. #142
    run(storage, session, "CREATE TABLE ev3 (id int PRIMARY KEY, at timestamptz)")
    run(storage, session, "CREATE INDEX ix_ev3_at ON ev3 (at)")
    run(storage, session, "INSERT INTO ev3 VALUES (1, '2020-01-02T03:04:05+00:00')")
    assert run(
        storage, session, "SELECT id FROM ev3 WHERE at = '2020-01-02T03:04:05+00:00'"
    ).rows == [(1,)]


# --------------------------------------------------------------------------- #
# #143: distinct naive `timestamp` (without time zone) type, OID 1114
# --------------------------------------------------------------------------- #


def test_timestamp_column_is_naive(storage, session):
    run(storage, session, "CREATE TABLE tsn (id int PRIMARY KEY, ts timestamp, tstz timestamptz)")
    run(
        storage,
        session,
        "INSERT INTO tsn VALUES (1, '2020-01-02T03:04:05', '2020-01-02T03:04:05+00:00')",
    )
    r = run(storage, session, "SELECT ts, tstz FROM tsn WHERE id = 1")
    assert [c.type_tag for c in r.columns] == ["timestamp", "timestamptz"]
    ts, tstz = r.rows[0]
    assert ts == dt.datetime(2020, 1, 2, 3, 4, 5) and ts.tzinfo is None  # naive
    assert tstz == dt.datetime(2020, 1, 2, 3, 4, 5, tzinfo=dt.timezone.utc)  # aware


def test_timestamp_column_oid_and_typename(storage, session):
    run(storage, session, "CREATE TABLE tso (id int PRIMARY KEY, ts timestamp)")
    r = run(
        storage,
        session,
        "SELECT data_type FROM information_schema.columns "
        "WHERE table_name = 'tso' AND column_name = 'ts'",
    )
    assert r.rows == [("timestamp without time zone",)]
    # RowDescription OID for the timestamp column is 1114 (not 1184).
    from secantus.sql import typemap

    assert typemap.PG_OID["timestamp"] == 1114
    assert r.columns  # column present


def test_timestamp_literal_drops_offset(storage, session):
    # `timestamp '…+offset'` keeps the wall-clock fields and drops the offset.
    v = val(storage, session, "SELECT timestamp '2020-06-15 12:00:00+05:00'")
    assert v == dt.datetime(2020, 6, 15, 12, 0, 0)
    assert v.tzinfo is None


def test_cast_timestamptz_to_timestamp_strips_tz(storage, session):
    v = val(storage, session, "SELECT (timestamptz '2020-06-15 12:00:00+00')::timestamp")
    assert v == dt.datetime(2020, 6, 15, 12, 0, 0)
    assert v.tzinfo is None


def test_timestamp_array_naive(storage, session):
    run(storage, session, "CREATE TABLE tsarr (id int PRIMARY KEY, ts timestamp[])")
    run(
        storage,
        session,
        "INSERT INTO tsarr VALUES (1, ARRAY['2020-01-02T03:04:05'::timestamp, "
        "'2021-06-07T08:09:10'::timestamp])",
    )
    v = val(storage, session, "SELECT ts FROM tsarr WHERE id = 1")
    assert list(v) == [dt.datetime(2020, 1, 2, 3, 4, 5), dt.datetime(2021, 6, 7, 8, 9, 10)]
    assert all(x.tzinfo is None for x in v)


# --------------------------------------------------------------------------- #
# PG-range values beyond Python's datetime limits, special values, loose input
# --------------------------------------------------------------------------- #


def test_parse_date_special_values():
    assert datetimes.parse_date("infinity") == "infinity"
    assert datetimes.parse_date("-infinity") == "-infinity"
    assert datetimes.parse_date("epoch") == "1970-01-01"
    assert datetimes.parse_date("1000-01-01 BC") == "1000-01-01 BC"
    assert datetimes.parse_date("10000-01-01") == "10000-01-01"
    assert datetimes.parse_date("2000-1-1") == "2000-01-01"


def test_parse_iso_datetime_loose_spellings():
    assert datetimes.parse_iso_datetime("2000-1-1") == dt.datetime(2000, 1, 1)
    assert datetimes.parse_iso_datetime("0258-1-8 1:12:32") == dt.datetime(258, 1, 8, 1, 12, 32)
    assert datetimes.parse_iso_datetime("2000-01-01 00:00+2") == dt.datetime(
        2000, 1, 1, tzinfo=dt.timezone(dt.timedelta(hours=2))
    )
    assert datetimes.parse_iso_datetime("epoch") == dt.datetime(1970, 1, 1, tzinfo=dt.timezone.utc)
    off = dt.timezone(dt.timedelta(hours=1, minutes=2, seconds=3))
    assert datetimes.parse_iso_datetime("2000-01-01 12:00:00+01:02:03") == dt.datetime(
        2000, 1, 1, 12, tzinfo=off
    )


def test_wide_timestamp_text_and_micros():
    assert datetimes.wide_timestamp_text("10000-01-01 12:00") == "10000-01-01 12:00:00"
    assert datetimes.wide_timestamp_text("1000-01-01 12:00 BC") == "1000-01-01 12:00:00 BC"
    assert datetimes.wide_timestamp_text("2000-01-01") is None
    # 10000-01-01 00:00 is exactly one day after 9999-12-31 24:00.
    max_micros = datetimes.wide_timestamp_micros("10000-01-01 00:00:00")
    from_max = (
        dt.datetime(9999, 12, 31, tzinfo=dt.timezone.utc)
        - dt.datetime(2000, 1, 1, tzinfo=dt.timezone.utc)
    ) // dt.timedelta(microseconds=1) + 86_400_000_000
    assert max_micros == from_max


def test_gregorian_ordinal_matches_python_in_range():
    for y, m, d in [(1, 1, 1), (2000, 2, 29), (9999, 12, 31), (1970, 1, 1)]:
        assert datetimes.gregorian_ordinal(y, m, d) == dt.date(y, m, d).toordinal()
        assert datetimes.ordinal_to_gregorian(dt.date(y, m, d).toordinal()) == (y, m, d)


def test_date_arithmetic_over_year_9999(tmp_path):
    st = Storage(str(tmp_path))
    s = Session(database=DB, user="secantus")
    try:
        res = run_sql(st, DB, "SELECT '9999-12-31'::date + 1", session=s)[-1]
        assert res.rows == [("10000-01-01",)]
        res = run_sql(st, DB, "SELECT 'infinity'::date", session=s)[-1]
        assert res.rows == [("infinity",)]
        res = run_sql(st, DB, "SELECT '1000-01-01 12:00 BC'::timestamp", session=s)[-1]
        assert res.rows == [("1000-01-01 12:00:00 BC",)]
    finally:
        st.close()


def test_tzinfo_for_setting_posix_inversion():
    plus2 = datetimes.tzinfo_for_setting("-02:00")
    assert dt.datetime(2000, 1, 1, tzinfo=plus2).utcoffset() == dt.timedelta(hours=2)
    minus12 = datetimes.tzinfo_for_setting("12:00")
    assert dt.datetime(2000, 1, 1, tzinfo=minus12).utcoffset() == dt.timedelta(hours=-12)
    assert datetimes.tzinfo_for_setting("UTC") == dt.timezone.utc
    assert datetimes.tzinfo_for_setting("NOSUCH0") == dt.timezone.utc


def test_session_timezone_governs_timestamptz(tmp_path):
    st = Storage(str(tmp_path))
    s = Session(database=DB, user="secantus")
    try:
        run_sql(st, DB, "SET TimeZone = '-02:00'", session=s)
        res = run_sql(st, DB, "SELECT '2000-01-01'::timestamptz", session=s)[-1]
        # Wall clock in UTC+2 == 1999-12-31 22:00 UTC (embedded results are
        # normalized tz-aware UTC).
        assert res.rows == [(dt.datetime(1999, 12, 31, 22, 0, tzinfo=dt.timezone.utc),)]
    finally:
        st.close()


def test_interval_unit_abbreviations_and_comparison(tmp_path):
    from secantus.sql import intervals

    assert intervals.parse("1s") == intervals.parse("1 second")
    assert intervals.parse("2 ms")["interval"]["micros"] == 2000
    assert intervals.parse("1d 3h 4m 5s")["interval"] == {
        "months": 0,
        "days": 1,
        "micros": (3 * 3600 + 4 * 60 + 5) * 1_000_000,
    }
    st = Storage(str(tmp_path))
    s = Session(database=DB, user="secantus")
    try:
        res = run_sql(
            st, DB, "SELECT '-1 day 23:59:59.999999'::interval = '-0.000001 s'::interval", session=s
        )[-1]
        assert res.rows == [(True,)]
    finally:
        st.close()
