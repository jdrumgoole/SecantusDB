"""``timestamptz`` respects the session time zone, both ways.

Three separate faults on one type:

* An offset-less literal was read as UTC. It is LOCAL TIME IN THE SESSION ZONE
  — `'1950-02-07'` under `America/New_York` is midnight in New York — so the
  stored instant was off by the zone's offset, and the same date came back a
  day early or late depending on which side of Greenwich the client sat.
* `GMT+13` did not resolve at all, falling back to UTC. Postgres keeps the
  POSIX sign there, so it means UTC-13 — getting that backwards doubles the
  error rather than fixing it.
* Offsets rendered as `+00:00` where Postgres writes `+00`. Clients compare the
  rendered text, so the trailing `:00` is not cosmetic.

Every expectation was checked against a real PostgreSQL 14.13.
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql import typemap
from secantus.sql.datetimes import tzinfo_for_setting
from secantus.sql.session import Session


@pytest.fixture()
def bind():
    """Bind a session with the given zone to the render context."""

    def _bind(zone: str) -> Session:
        session = Session(database="t")
        session.settings["TimeZone"] = zone
        typemap.set_render_session(session)
        return session

    try:
        yield _bind
    finally:
        typemap.set_render_session(None)


def _offset(zone: str, when: dt.datetime = dt.datetime(2020, 1, 1)) -> str:
    return when.replace(tzinfo=tzinfo_for_setting(zone)).strftime("%z")


class TestZoneResolution:
    @pytest.mark.parametrize(
        "zone,expected",
        [
            ("GMT+13", "-1300"),  # POSIX sign: GMT+13 is UTC-13
            ("GMT-13", "+1300"),
            ("UTC+5", "-0500"),
            ("GMT", "+0000"),
            ("+12", "-1200"),  # bare numeric, same convention
            ("-02:00", "+0200"),
            ("America/New_York", "-0500"),
        ],
    )
    def test_offsets(self, zone, expected):
        assert _offset(zone) == expected

    def test_beyond_the_etc_gmt_range(self):
        """zoneinfo's ``Etc/GMT±N`` stops at 12; Postgres accepts more, so the
        offset is built directly rather than looked up."""
        assert _offset("GMT+13") == "-1300"

    def test_unknown_zone_falls_back_to_utc(self):
        assert _offset("Not/AZone") == "+0000"


class TestInputIsLocalTime:
    @pytest.mark.parametrize(
        "zone,expected_utc",
        [
            ("GMT", dt.datetime(1950, 2, 7, 0, 0)),
            ("America/New_York", dt.datetime(1950, 2, 7, 5, 0)),  # midnight EST
            ("GMT+13", dt.datetime(1950, 2, 7, 13, 0)),  # midnight at UTC-13
        ],
    )
    def test_offsetless_literal_is_session_local(self, bind, zone, expected_utc):
        bind(zone)
        got = typemap.coerce("1950-02-07", "timestamptz")
        assert got.astimezone(dt.timezone.utc).replace(tzinfo=None) == expected_utc

    def test_explicit_offset_is_absolute(self, bind):
        """A literal that carries its own offset is already an instant and must
        not be re-interpreted."""
        bind("America/New_York")
        got = typemap.coerce("1950-02-07 00:00:00+02", "timestamptz")
        assert got.astimezone(dt.timezone.utc).replace(tzinfo=None) == dt.datetime(
            1950, 2, 6, 22, 0
        )


class TestRendering:
    @pytest.mark.parametrize(
        "zone,expected",
        [
            ("GMT", "2005-01-01 12:00:00+00"),
            ("GMT+13", "2005-01-01 12:00:00-13"),
            ("America/New_York", "2005-01-01 12:00:00-05"),
            ("Asia/Kolkata", "2005-01-01 12:00:00+05:30"),  # half-hour keeps :30
        ],
    )
    def test_round_trip_in_the_session_zone(self, bind, zone, expected):
        bind(zone)
        value = typemap.coerce("2005-01-01 12:00:00", "timestamptz")
        assert typemap.to_pg_text(value, "timestamptz").decode() == expected


class TestOtherTypesAreZoneIndependent:
    """A `date` or a `timestamp without time zone` must not move with the
    session zone — shifting those is how a plain calendar date loses a day."""

    @pytest.mark.parametrize("zone", ["GMT", "GMT+13", "America/New_York", "Australia/Sydney"])
    def test_date(self, bind, zone):
        # A date is carried as its calendar text, with no instant behind it to
        # shift — which is precisely the property being asserted here.
        bind(zone)
        assert typemap.coerce("1950-02-07", "date") == "1950-02-07"

    @pytest.mark.parametrize("zone", ["GMT", "GMT+13", "America/New_York", "Australia/Sydney"])
    def test_timestamp(self, bind, zone):
        bind(zone)
        assert typemap.coerce("1950-02-07 00:00:00", "timestamp") == dt.datetime(1950, 2, 7, 0, 0)
