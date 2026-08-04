"""``1950-02-07 -05`` — a date with an offset and no time of day.

This is what a JDBC ``setDate`` with a Calendar puts on the wire, which only
became visible by decoding Bind messages in the capture proxy.

Widening it for ``fromisoformat`` dropped the space, leaving
``1950-02-07-05:00``, which Python reads as the TIME 05:00 rather than an
offset: the value silently became five in the morning, tz-naive. Postgres reads
the implicit midnight, so the offset is now spelled out against an explicit
``00:00:00``.

Expectations checked against a live PostgreSQL 14.13 under ``TimeZone=UTC``:

    '1950-02-07 -05'::date        -> 1950-02-07
    '1950-02-07 -05'::timestamp   -> 1950-02-07 00:00:00
    '1950-02-07 -05'::timestamptz -> 1950-02-07 05:00:00+00
"""

from __future__ import annotations

import datetime as dt

import bson
import pytest

from secantus.sql import typemap
from secantus.sql.datetimes import parse_iso_datetime
from secantus.sql.session import Session

MINUS_5 = dt.timezone(dt.timedelta(hours=-5))


@pytest.fixture()
def utc_session():
    session = Session(database="t")
    session.settings["TimeZone"] = "UTC"
    typemap.set_render_session(session)
    try:
        yield session
    finally:
        typemap.set_render_session(None)


class TestDateWithOffsetAndNoTime:
    def test_parses_to_midnight_at_that_offset(self):
        assert parse_iso_datetime("1950-02-07 -05") == dt.datetime(1950, 2, 7, tzinfo=MINUS_5)

    def test_date_column_keeps_the_calendar_day(self, utc_session):
        assert typemap.coerce("1950-02-07 -05", "date") == "1950-02-07"

    def test_timestamp_column_drops_the_offset(self, utc_session):
        assert typemap.coerce("1950-02-07 -05", "timestamp") == dt.datetime(1950, 2, 7, 0, 0)

    def test_timestamptz_column_keeps_the_instant(self, utc_session):
        got = typemap.coerce("1950-02-07 -05", "timestamptz")
        assert got.astimezone(dt.timezone.utc) == dt.datetime(
            1950, 2, 7, 5, 0, tzinfo=dt.timezone.utc
        )

    @pytest.mark.parametrize(
        "text,expected",
        [
            ("1950-02-07 +02", dt.datetime(1950, 2, 7, tzinfo=dt.timezone(dt.timedelta(hours=2)))),
            (
                "1950-02-07 -05:30",
                dt.datetime(1950, 2, 7, tzinfo=dt.timezone(-dt.timedelta(hours=5, minutes=30))),
            ),
        ],
    )
    def test_other_offset_spellings(self, text, expected):
        assert parse_iso_datetime(text) == expected

    def test_a_date_with_a_time_is_unaffected(self):
        assert parse_iso_datetime("1950-02-07 12:34:56 -05") == dt.datetime(
            1950, 2, 7, 12, 34, 56, tzinfo=MINUS_5
        )

    def test_a_bare_date_is_unaffected(self):
        assert parse_iso_datetime("1950-02-07") == dt.datetime(1950, 2, 7, 0, 0)


class TestExtremesStayStorable:
    """Parsing an offset where none was parsed before pushed values near
    ``datetime.min`` out of range: BSON normalises an aware datetime to UTC,
    and ``0001-01-01 00:00+05:00`` is year zero there. Those keep their wall
    clock rather than raising, which is what they did before."""

    @pytest.mark.parametrize(
        "text", ["0001-01-01 +05", "0001-01-01 -05", "9999-12-31 -05", "9999-12-31 +05"]
    )
    def test_encodable(self, utc_session, text):
        value = typemap.coerce(text, "timestamptz")
        bson.encode({"v": value})  # must not raise

    def test_ordinary_values_keep_their_offset(self, utc_session):
        value = typemap.coerce("1950-02-07 -05", "timestamptz")
        assert value.tzinfo is not None
