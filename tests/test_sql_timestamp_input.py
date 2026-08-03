"""Leap seconds roll forward; malformed timestamps report a syntax error.

Two problems on one path, both reaching the client as ``XX000 internal
error``: Python's ``datetime`` rejects second 60 outright, and the
``ValueError`` from *any* unparseable timestamp escaped uncaught.

Every expectation here was checked against a real PostgreSQL 14.13.
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql.datetimes import parse_iso_datetime
from secantus.sql.engine import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def q(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run("CREATE TABLE tt (id int primary key, ts timestamp, tz timestamptz)")
    try:
        yield run
    finally:
        storage.close()


class TestLeapSecondRollsForward:
    @pytest.mark.parametrize(
        "text,expected",
        [
            ("2015-06-30 23:59:60", dt.datetime(2015, 7, 1, 0, 0)),
            ("2015-12-31 23:59:60", dt.datetime(2016, 1, 1, 0, 0)),  # year boundary
            ("2015-06-30 12:30:60", dt.datetime(2015, 6, 30, 12, 31)),  # plain minute
        ],
    )
    def test_parses_to_the_next_second(self, text, expected):
        assert parse_iso_datetime(text) == expected

    def test_offset_is_preserved(self):
        got = parse_iso_datetime("2015-06-30 23:59:60+02")
        assert got == dt.datetime(2015, 7, 1, 0, 0, tzinfo=dt.timezone(dt.timedelta(hours=2)))

    def test_through_a_stored_column(self, q):
        q("INSERT INTO tt (id, ts) VALUES (1, '2015-06-30 23:59:60')")
        assert q("SELECT ts FROM tt") == [(dt.datetime(2015, 7, 1, 0, 0),)]


class TestOutOfRangeIsAnError:
    """Postgres accepts exactly ``:60`` with no fraction and rejects the rest."""

    @pytest.mark.parametrize(
        "text", ["2015-06-30 23:59:61", "2015-06-30 23:59:60.5", "not-a-date", ""]
    )
    def test_reports_invalid_input_syntax(self, q, text):
        with pytest.raises(Exception) as exc:
            q(f"INSERT INTO tt (id, ts) VALUES (1, '{text}')")
        assert getattr(exc.value, "sqlstate", None) == "22P02", (
            f"{text!r} should be a syntax error, not {exc.value!r}"
        )

    def test_a_valid_timestamp_still_works(self, q):
        q("INSERT INTO tt (id, ts) VALUES (1, '2015-06-30 12:00:00')")
        assert q("SELECT ts FROM tt") == [(dt.datetime(2015, 6, 30, 12, 0),)]
