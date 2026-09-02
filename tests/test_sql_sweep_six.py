"""A sixth sweep — constraints, identity, time zones, GUCs, string functions.

20 of 33 shapes matched PostgreSQL 14.13. The important miss was silently
wrong arithmetic:

`age('2021-03-15', '2020-01-20')` answered `1 year 1 mon 23 days` where
PostgreSQL answers `26 days`. When the day difference goes negative, the borrow
takes the length of the **start** date's month — January's 31 here — not the
month before `end` (February's 28), and not a flat 31. Eight probed cases
discriminate all three readings, including `age('2020-04-01','2020-01-15')`
(January's 31, not April's 30) and `age('2020-03-01','2020-02-28')`
(February's 29, not 31).

`format('%1$s-%1$s-%2$s', 'a', 'b')` returned the format string unchanged —
positional specifiers were not recognised, so the whole directive was copied
through as literal text.

`current_setting('nope', true)` answered the empty string, which reads as a
setting that exists and is blank; PostgreSQL answers NULL, and without the
`missing_ok` flag raises `42704`.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

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
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0][0][0]

    try:
        yield run
    finally:
        storage.close()


class TestAgeBorrow:
    @pytest.mark.parametrize(
        ("end", "start", "want"),
        [
            # The three cases that discriminate "start's month" from "the month
            # before end" and from a flat 31.
            ("2020-03-01", "2020-01-15", "1 mon 17 days"),
            ("2020-04-01", "2020-01-15", "2 mons 17 days"),
            ("2020-03-01", "2020-02-28", "2 days"),
            # The rest of the probed corpus.
            ("2021-03-15", "2020-01-20", "1 year 1 mon 26 days"),
            ("2020-03-15", "2020-01-15", "2 mons"),
            ("2020-03-01", "2020-02-01", "1 mon"),
            ("2021-01-01", "2020-01-15", "11 mons 17 days"),
            ("2020-01-31", "2020-01-01", "30 days"),
            ("2020-03-31", "2020-01-31", "2 mons"),
            ("2020-03-01", "2020-01-31", "1 mon 1 day"),
            ("2019-03-01", "2019-01-15", "1 mon 17 days"),
            ("2020-02-01", "2020-01-15", "17 days"),
            ("2020-02-01", "2019-12-15", "1 mon 17 days"),
            ("2020-02-28", "2019-12-30", "1 mon 29 days"),
            ("2020-06-01", "2020-05-31", "1 day"),
            ("2020-07-01", "2020-06-30", "1 day"),
        ],
    )
    def test_age(self, db, end, start, want):
        assert db(f"SELECT age(timestamp '{end}', timestamp '{start}')::text") == want


class TestFormatPositional:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT format('%1$s-%1$s-%2$s', 'a', 'b')", "a-a-b"),
            ("SELECT format('%2$L', 'a', 'b')", "'b'"),
            ("SELECT format('%1$I', 'a b')", '"a b"'),
            # The sequential forms were always right.
            ("SELECT format('%s-%s', 'a', 'b')", "a-b"),
            ("SELECT format('%I', 'a b')", '"a b"'),
            ("SELECT format('100%%')", "100%"),
        ],
    )
    def test_format(self, db, sql, want):
        assert db(sql) == want


class TestCurrentSetting:
    def test_missing_ok_is_null(self, db):
        assert db("SELECT current_setting('nope', true)") is None

    def test_missing_without_the_flag_errors(self, db):
        with pytest.raises(SQLError) as exc:
            db("SELECT current_setting('nope')")
        assert exc.value.sqlstate == "42704"
        assert exc.value.message == 'unrecognized configuration parameter "nope"'

    def test_known_setting(self, db):
        assert db("SELECT current_setting('search_path')") == '"$user", public'


class TestNewBuiltins:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            # An unquoted part folds to lower case; a quoted one keeps its case.
            ("SELECT parse_ident('a.b')", ["a", "b"]),
            ("SELECT parse_ident('a.B')", ["a", "b"]),
            ("""SELECT parse_ident('"A".b')""", ["A", "b"]),
            ("SELECT unistr('\\0041')", "A"),
            ("SELECT normalize('abc')", "abc"),
        ],
    )
    def test_values(self, db, sql, want):
        assert db(sql) == want


class TestLocaltimestamp:
    def test_follows_the_session_zone(self, db):
        """`datetime.now()` is the MACHINE's wall clock, a different instant
        from the session's whenever the zones differ — on a UTC+1 host with the
        default UTC session this was FALSE."""
        assert db("SELECT localtimestamp <= now()") is True

    def test_nested_in_an_expression(self, db):
        """It worked as a bare projection through the session-function path but
        reported `function localtimestamp() is not supported` once nested."""
        assert db("SELECT localtimestamp IS NOT NULL") is True
