"""``to_char`` / ``to_date`` / ``to_timestamp`` datetime templates.

Every expectation here was measured against PostgreSQL 14.13 under
``SET TIME ZONE 'UTC'`` — none of it is derived from this implementation's own
output. Two differential sweeps (314 cases) back the table.
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql import run_sql
from secantus.sql.datetimeformat import parse_datetime, to_char_datetime
from secantus.sql.session import Session
from secantus.storage import Storage

TS = dt.datetime(2026, 9, 2, 14, 7, 5, 123456, tzinfo=dt.timezone.utc)


@pytest.fixture
def store(tmp_path):
    s = Storage(str(tmp_path / "dtf"))
    try:
        yield s
    finally:
        s.close()


def _one(store, sql):
    return [r for r in run_sql(store, "t", sql, session=Session(database="t"))][0].rows[0][0]


@pytest.mark.parametrize(
    ("fmt", "want"),
    [
        # --- tokens that used to pass through as their own spelling ---------
        ("Q", "3"),
        ("W", "1"),
        ("WW", "35"),
        ("CC", "21"),
        ("J", "2461286"),
        ("MS", "123"),
        ("US", "123456"),
        ("SSSS", "50825"),
        ("SSSSS", "50825"),
        ("HH", "02"),
        ("RM", "IX  "),  # blank-padded to 4 on the RIGHT
        ("rm", "ix  "),
        ("Y,YYY", "2,026"),
        ("YYY", "026"),
        ("Y", "6"),
        ("FF1", "1"),
        ("FF3", "123"),
        ("FF6", "123456"),
        ("TZH", "+00"),
        ("TZM", "00"),
        ("OF", "+00"),
        # --- ISO week-numbering --------------------------------------------
        ("IYY", "026"),
        ("IY", "26"),
        ("I", "6"),
        ("IDDD", "248"),
        ("IYYY-IW-ID", "2026-36-3"),
        # --- D is 1=Sunday..7=Saturday, NOT the ISO weekday -----------------
        ("D", "4"),
        ("ID", "3"),
        ("DDD", "245"),
        # --- case comes from the token's own spelling ----------------------
        ("Month", "September"),
        ("MONTH", "SEPTEMBER"),
        ("month", "september"),
        ("Mon", "Sep"),
        ("MON", "SEP"),
        ("mon", "sep"),
        ("Day", "Wednesday"),
        ("DAY", "WEDNESDAY"),
        ("day", "wednesday"),
        ("Dy", "Wed"),
        ("DY", "WED"),
        ("dy", "wed"),
        ("am", "pm"),
        ("AM", "PM"),
        ("A.M.", "P.M."),
        ("a.m.", "p.m."),
        ("AD", "AD"),
        ("BC", "AD"),
        ("ad", "ad"),
        ("A.D.", "A.D."),
        ("B.C.", "A.D."),
        # --- quoted literals ------------------------------------------------
        ('"Year:" YYYY', "Year: 2026"),
        ('"a"YYYY"b"', "a2026b"),
        ('YYYY-MM-DD"T"HH24:MI:SS', "2026-09-02T14:07:05"),
        ('YYYY-"W"IW', "2026-W36"),
        # --- TH / th ordinals -----------------------------------------------
        ("DDth", "02nd"),
        ("DDTH", "02ND"),
        ("ddth", "02nd"),
        # --- FM prefixes ONE token, it is not a mode that stays on ----------
        ("FMMonth FMDD, YYYY", "September 2, 2026"),
        ("FMHH12:MI", "2:07"),
        ("FMMM/FMDD", "9/2"),
        ("FMDay", "Wednesday"),
        ("TMMonth", "September"),
        # --- unregistered spellings fall through as literal text ------------
        ("DS", "4S"),
        ("DL", "4L"),
        ("DDth of Month", "02nd of September"),
        # --- whole templates -------------------------------------------------
        ("YYYY-MM-DD HH24:MI:SS", "2026-09-02 14:07:05"),
        ("YYYY-MM-DD HH:MI:SS AM", "2026-09-02 02:07:05 PM"),
        ("HH12:MI:SS.MS", "02:07:05.123"),
        ("W-WW-IW", "1-35-36"),
        ("Q-DDD", "3-245"),
    ],
)
def test_to_char_datetime_tokens(fmt, want):
    assert to_char_datetime(TS, fmt) == want


def test_ddth_is_two_separate_D_tokens():
    """Postgres matches template tokens case-SENSITIVELY.

    ``Dd`` is not a registered spelling, so ``Ddth`` is ``D`` (weekday 4) then
    ``d`` (weekday 4 again) then ``th`` — ``'44th'``. Matching the table
    case-insensitively made it ``DD`` and answered ``'02nd'``.
    """
    assert to_char_datetime(TS, "Ddth") == "44th"
    assert to_char_datetime(TS, "ddth") == "02nd"


def test_tz_name_is_empty_for_a_plain_timestamp(store):
    """A `timestamp` has no zone to NAME, but the numeric offset tokens still
    report the session offset — measured, and inconsistent-looking on purpose."""
    assert _one(store, "SELECT to_char(TIMESTAMP '2026-09-02 14:07:05', 'TZ')") == ""
    assert _one(store, "SELECT to_char(TIMESTAMP '2026-09-02 14:07:05', 'OF')") == "+00"
    # `OF` / `TZH` / `TZM` are the only tokens with NO lower-case spelling, so
    # `tzh` is the TZ token (empty here) followed by a literal 'h'.
    assert _one(store, "SELECT to_char(TIMESTAMP '2026-09-02 14:07:05', 'tzh')") == "h"


@pytest.mark.parametrize(
    ("text", "fmt", "want"),
    [
        ("2026-09-02", "YYYY-MM-DD", dt.datetime(2026, 9, 2)),
        ("02/09/2026", "DD/MM/YYYY", dt.datetime(2026, 9, 2)),
        ("Sep 2, 2026", "Mon DD, YYYY", dt.datetime(2026, 9, 2)),
        ("September 2, 2026", "Month DD, YYYY", dt.datetime(2026, 9, 2)),
        ("Wed Sep 02 2026", "Dy Mon DD YYYY", dt.datetime(2026, 9, 2)),
        ("2026-245", "YYYY-DDD", dt.datetime(2026, 9, 2)),
        ("20260902", "YYYYMMDD", dt.datetime(2026, 9, 2)),
        ("2026/9/2", "YYYY/MM/DD", dt.datetime(2026, 9, 2)),
        ("2461286", "J", dt.datetime(2026, 9, 2)),
        # 2-digit year: 00-69 is 2000s, 70-99 is 1900s.
        ("99", "YY", dt.datetime(1999, 1, 1)),
        ("26-09-02", "YY-MM-DD", dt.datetime(2026, 9, 2)),
        # ISO week-numbering: week 33 of 2026 starts Monday 2026-08-10.
        ("2026 33", "IYYY IW", dt.datetime(2026, 8, 10)),
        ("2026 33 3", "IYYY IW ID", dt.datetime(2026, 8, 12)),
        ("2026-09-02 14:07:05", "YYYY-MM-DD HH24:MI:SS", dt.datetime(2026, 9, 2, 14, 7, 5)),
        (
            "2026-09-02 02:07:05 PM",
            "YYYY-MM-DD HH12:MI:SS AM",
            dt.datetime(2026, 9, 2, 14, 7, 5),
        ),
        (
            "2026-09-02 14:07:05.123",
            "YYYY-MM-DD HH24:MI:SS.MS",
            dt.datetime(2026, 9, 2, 14, 7, 5, 123000),
        ),
    ],
)
def test_parse_datetime(text, fmt, want):
    assert parse_datetime(text, fmt) == want


def test_to_timestamp_is_a_timestamptz(store):
    """`to_timestamp` returns a timestamptz — it was returned naive, so it
    rendered without the `+00` Postgres sends."""
    assert (
        _one(store, "SELECT (to_timestamp('2026-09-02 14:07:05','YYYY-MM-DD HH24:MI:SS'))::text")
        == "2026-09-02 14:07:05+00"
    )
    assert _one(store, "SELECT (to_timestamp('99','YY'))::text") == "1999-01-01 00:00:00+00"


def test_word_templates_parse_end_to_end(store):
    """These raised `22007 invalid input syntax` before: sqlglot's strftime
    mapping knows no `Mon` and no `IW`, so the template never matched."""
    assert _one(store, "SELECT to_date('Sep 2, 2026','Mon DD, YYYY')") == dt.date(2026, 9, 2)
    assert _one(store, "SELECT to_date('2026 33','IYYY IW')") == dt.date(2026, 8, 10)
    assert _one(store, "SELECT to_date('September 2, 2026','Month DD, YYYY')") == dt.date(
        2026, 9, 2
    )
