"""Converting a ``timestamptz`` value into ``timestamp`` uses the session zone.

Postgres draws a line that is easy to miss, and both halves were checked
against a live PostgreSQL 14.13 under ``America/New_York``:

    '1950-02-07 00:00:00+02'::timestamp              -> 1950-02-07 00:00:00
    ('1950-02-07 00:00:00+02'::timestamptz)::timestamp -> 1950-02-06 17:00:00

A text LITERAL has its offset discarded and its wall-clock fields kept. A
``timestamptz`` VALUE is a real instant, so narrowing it to ``timestamp``
converts through the session zone first.

We applied the literal rule to both, so an aware value kept UTC wall clock and
every such timestamp was off by the session zone's offset.
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql import typemap
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


class TestAwareValueConvertsThroughTheSessionZone:
    @pytest.mark.parametrize(
        "zone,expected",
        [
            ("America/New_York", dt.datetime(1950, 2, 6, 17, 0)),
            ("UTC", dt.datetime(1950, 2, 6, 22, 0)),
            ("GMT-2", dt.datetime(1950, 2, 7, 0, 0)),  # same zone as the value
        ],
    )
    def test_value_cast(self, bind, zone, expected):
        bind(zone)
        aware = dt.datetime(1950, 2, 7, 0, 0, tzinfo=dt.timezone(dt.timedelta(hours=2)))
        assert typemap.coerce(aware, "timestamp") == expected

    def test_naive_value_is_left_alone(self, bind):
        """Nothing to convert — a naive value is already wall clock."""
        bind("America/New_York")
        naive = dt.datetime(1950, 2, 7, 0, 0)
        assert typemap.coerce(naive, "timestamp") == naive


class TestTextLiteralKeepsItsWallClock:
    @pytest.mark.parametrize("zone", ["America/New_York", "UTC", "GMT+13"])
    def test_offset_is_discarded_not_applied(self, bind, zone):
        """Postgres' timestamp INPUT rule, and it must not follow the session
        zone — otherwise the same literal means different things per client."""
        bind(zone)
        assert typemap.coerce("1950-02-07 00:00:00+02", "timestamp") == dt.datetime(
            1950, 2, 7, 0, 0
        )

    @pytest.mark.parametrize("zone", ["America/New_York", "UTC", "GMT+13"])
    def test_offsetless_literal_unchanged(self, bind, zone):
        bind(zone)
        assert typemap.coerce("1950-02-07 00:00:00", "timestamp") == dt.datetime(1950, 2, 7, 0, 0)
