"""`to_char` on an interval, word-token casing and padding, `to_date` /
`to_timestamp`, and three missing `extract` fields.

Found 2026-09-01 by a date/interval sweep against PostgreSQL 14.13:

* **`to_char(interval '3 days', 'DD')` reached the wire as `XX000 internal
  error`** — `to_char` assumed a datetime, and an interval is a subdocument, so
  it fell through to the ISO parser which raised `ValueError`. The second
  leaked internal error found this session.
* `to_char(date, 'Day')` did not blank-pad and `'DY'` did not upper-case.
* `to_date` / `to_timestamp` were absent, and said so naming a function the
  user never wrote (`str_to_date`) because sqlglot renames them.
* `extract(century | millennium | decade)` were unsupported.
"""

from __future__ import annotations

import datetime as dt

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        res = [r for r in run_sql(storage, "t", sql, session=session)][0]
        return res.rows, [c.type_tag for c in res.columns]

    try:
        yield run
    finally:
        storage.close()


class TestToCharInterval:
    @pytest.mark.parametrize(
        ("value", "fmt", "want"),
        [
            ("3 days", "DD", "03"),
            ("40 days", "DD", "40"),  # days are NOT wrapped into months
            ("5 hours", "HH24", "05"),
            ("30 hours", "HH24", "30"),  # nor hours into days
            ("7 minutes", "MI", "07"),
            ("9 seconds", "SS", "09"),
            ("2 months", "MM", "02"),
            ("14 months", "MM", "02"),  # months ARE mod 12 ...
            ("14 months", "YYYY", "0001"),  # ... with the carry in years
            ("1 year", "YYYY", "0001"),
            ("90 minutes", "HH24:MI", "01:30"),
            ("3661 seconds", "HH24:MI:SS", "01:01:01"),
            ("1 day 25 hours", "DD HH24", "01 25"),
            ("-3 days", "DD", "-3"),  # a negative field keeps its sign, unpadded
            ("1 year 2 mons 3 days 04:05:06", "YYYY-MM-DD HH24:MI:SS", "0001-02-03 04:05:06"),
        ],
    )
    def test_field_templates(self, db, value, fmt, want):
        assert db(f"SELECT to_char(interval '{value}', '{fmt}')")[0] == [(want,)]

    @pytest.mark.parametrize("fmt", ["Day", "DY", "Month"])
    def test_calendar_templates_are_22007(self, db, fmt):
        """PG: "Intervals are not tied to specific calendar dates"."""
        with pytest.raises(SQLError) as ei:
            db(f"SELECT to_char(interval '3 days', '{fmt}')")
        assert ei.value.sqlstate == "22007"

    def test_dd_is_not_read_as_the_rejected_d_token(self, db):
        """`DD` contains `D`, which IS rejected — a substring test made every
        `DD` format a spurious 22007."""
        assert db("SELECT to_char(interval '3 days', 'DD')")[0] == [("03",)]


class TestWordTokenCasingAndPadding:
    @pytest.mark.parametrize(
        ("fmt", "want"),
        [
            ("Day", "Thursday "),  # blank-padded to 9
            ("DAY", "THURSDAY "),
            ("Dy", "Thu"),
            ("DY", "THU"),
            ("Month", "March    "),
            ("MONTH", "MARCH    "),
            ("Mon", "Mar"),
            ("MON", "MAR"),
            ("FMDay", "Thursday"),  # FM suppresses the padding
            ("YYYY-MM-DD Day", "2020-03-05 Thursday "),
            ("Dy DD Mon YYYY", "Thu 05 Mar 2020"),
        ],
    )
    def test_case_and_padding(self, db, fmt, want):
        assert db(f"SELECT to_char(date '2020-03-05', '{fmt}')")[0] == [(want,)]


class TestExtractFields:
    @pytest.mark.parametrize(
        ("field", "date", "want"),
        [
            ("century", "2020-01-01", 21),
            ("century", "1900-01-01", 19),  # the boundary year belongs to the previous century
            ("millennium", "2020-01-01", 3),
            ("decade", "2020-01-01", 202),
        ],
    )
    def test_fields(self, db, field, date, want):
        assert db(f"SELECT extract({field} from date '{date}')")[0] == [(want,)]


class TestToDateAndToTimestamp:
    @pytest.mark.parametrize(
        ("expr", "value", "tag"),
        [
            ("to_date('2020-03-05','YYYY-MM-DD')", dt.date(2020, 3, 5), "date"),
            ("to_date('05/03/2020','DD/MM/YYYY')", dt.date(2020, 3, 5), "date"),
            (
                "to_timestamp(0)",
                dt.datetime(1970, 1, 1, tzinfo=dt.timezone.utc),
                "timestamptz",
            ),
            (
                "to_timestamp(1577836800)",
                dt.datetime(2020, 1, 1, tzinfo=dt.timezone.utc),
                "timestamptz",
            ),
        ],
    )
    def test_value_and_type(self, db, expr, value, tag):
        rows, tags = db(f"SELECT {expr}")
        assert rows == [(value,)]
        assert tags == [tag]
