"""``SET TIME ZONE`` and ``SHOW TIME ZONE`` reach the TimeZone GUC.

The two-word spelling is what JDBC uses to pin a connection's zone, and it was
a complete no-op: ``SET TIME ZONE 'x'`` takes no ``=`` or ``TO``, so the
generic name-value fallback never matched it, and ``SHOW TIME ZONE`` answered
empty because ``"time zone"`` was not among the GUC aliases. A client that
configured its zone that way silently stayed on the default.

Note this only makes the SETTING stick. Converting a ``timestamptz`` into that
zone is a separate, larger piece of work — see tasks/backlog.md.
"""

from __future__ import annotations

import pytest

from secantus.sql.engine import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run.session = session  # type: ignore[attr-defined]
    try:
        yield run
    finally:
        storage.close()


class TestSetTimeZone:
    @pytest.mark.parametrize(
        "stmt,expected",
        [
            ("SET TIME ZONE 'GMT+13'", "GMT+13"),
            ("SET TIME ZONE 'America/New_York'", "America/New_York"),
            ("SET TIME ZONE UTC", "UTC"),
            ("SET SESSION TIME ZONE 'GMT+5'", "GMT+5"),
            ("SET timezone = 'GMT+5'", "GMT+5"),  # the form that already worked
        ],
    )
    def test_sets_the_guc(self, db, stmt, expected):
        db(stmt)
        assert db.session.get_setting("TimeZone") == expected

    def test_default_resets(self, db):
        db("SET TIME ZONE 'GMT+13'")
        db("SET TIME ZONE DEFAULT")
        assert db.session.get_setting("TimeZone") == "UTC"

    def test_parameter_status_announces_the_change(self, tmp_path):
        """TimeZone is a reportable GUC — clients track it via ParameterStatus,
        so the change has to be announced, not merely stored."""
        storage = Storage(str(tmp_path / "wt"))
        session = Session(database="t")
        try:
            result = run_sql(storage, "t", "SET TIME ZONE 'GMT+13'", session=session)[0]
            assert ("TimeZone", "GMT+13") in (result.parameter_status or [])
        finally:
            storage.close()


class TestShowTimeZone:
    def test_two_word_form(self, db):
        db("SET timezone = 'GMT+13'")
        assert db("SHOW TIME ZONE") == [("GMT+13",)]

    def test_one_word_form(self, db):
        db("SET TIME ZONE 'GMT+13'")
        assert db("SHOW timezone") == [("GMT+13",)]

    def test_current_setting_agrees(self, db):
        db("SET TIME ZONE 'America/New_York'")
        assert db("SELECT current_setting('TimeZone')") == [("America/New_York",)]

    def test_extra_whitespace_still_resolves(self, db):
        db("SET TIME ZONE 'GMT+13'")
        assert db("SHOW TIME  ZONE") == [("GMT+13",)]
