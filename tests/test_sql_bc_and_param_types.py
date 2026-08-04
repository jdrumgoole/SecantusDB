"""Two faults the pgjdbc wire capture isolated.

* A ``timestamp`` (without time zone) column kept an offset the input carried
  when the value was outside Python's datetime range, rendering
  ``0101-01-01 00:00:00+00 BC`` where Postgres writes
  ``0101-01-01 00:00:00 BC``. The in-range path already dropped it.
* A ``timestamptz``-declared bound parameter is substituted as
  ``CAST('…' AS timestamptz)``, and that cast evaluated to TEXT — so the
  declared type was lost and storing it into a ``timestamp`` column applied
  Postgres' literal rule instead of converting the value through the session
  zone.

Both checked against a live PostgreSQL 14.13.
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql import planner, typemap
from secantus.sql.session import Session


@pytest.fixture()
def bind():
    def _bind(zone: str) -> Session:
        session = Session(database="t")
        session.settings["TimeZone"] = zone
        typemap.set_render_session(session)
        return session

    try:
        yield _bind
    finally:
        typemap.set_render_session(None)


class TestBcAndWideTimestamps:
    @pytest.mark.parametrize(
        "text,expected",
        [
            ("0101-01-01 00:00:00+00 BC", "0101-01-01 00:00:00 BC"),
            ("0101-01-01 00:00:00 BC", "0101-01-01 00:00:00 BC"),
            ("10000-01-01 12:00:00+02", "10000-01-01 12:00:00"),
        ],
    )
    def test_timestamp_column_drops_the_offset(self, text, expected):
        assert typemap.coerce(text, "timestamp") == expected

    @pytest.mark.parametrize(
        "text,expected",
        [
            ("0101-01-01 00:00:00+00 BC", "0101-01-01 00:00:00+00 BC"),
            ("10000-01-01 12:00:00+02", "10000-01-01 12:00:00+02"),
        ],
    )
    def test_timestamptz_column_keeps_it(self, text, expected):
        assert typemap.coerce(text, "timestamptz") == expected


class TestTimestamptzCastResolvesToAnInstant:
    """``CAST('…' AS timestamptz)`` must produce a datetime, not text — that is
    how a bound parameter's declared type survives into column coercion."""

    def test_cast_yields_a_datetime(self, bind):
        bind("UTC")
        node = planner.exp.DataType.build("timestamptz")
        got = planner._coerce_cast("1950-02-07 05:00:00+00:00", node)
        assert isinstance(got, dt.datetime)
        assert got.tzinfo is not None

    def test_already_a_datetime_is_untouched(self, bind):
        bind("UTC")
        node = planner.exp.DataType.build("timestamptz")
        value = dt.datetime(1950, 2, 7, tzinfo=dt.timezone.utc)
        assert planner._coerce_cast(value, node) is value

    def test_the_resolved_instant_narrows_through_the_session_zone(self, bind):
        """The point of the whole chain: a timestamptz parameter stored into a
        timestamp column lands on session-local wall clock, not UTC."""
        bind("America/New_York")
        node = planner.exp.DataType.build("timestamptz")
        instant = planner._coerce_cast("1950-02-07 05:00:00+00:00", node)
        assert typemap.coerce(instant, "timestamp") == dt.datetime(1950, 2, 7, 0, 0)
