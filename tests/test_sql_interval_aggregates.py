"""`sum(interval)` answered `0` and `avg(interval)` answered NULL.

An interval rides as a subdocument, and Mongo's `$sum` over subdocuments is
`0` while its `$avg` is NULL — so both came back as silently wrong data rather
than an error. PostgreSQL gives `3 days` and `1 day 12:00:00`.

`min` / `max` were unaffected: Mongo's BSON order over the subdocument happens
to agree with duration order for these shapes.

The fold is Python-side. `intervals.add` is componentwise, which is what PG's
`interval_pl` does; the average divides the total, carrying months into days
and days into micros, which a per-field divide would get wrong.

A related rendering bug is fixed here too: an interval INSIDE an array came out
as its raw subdocument (`{"{\\"interval\\": {\\"months\\": 0, …}}"}`) because
the text cast defaulted every array element to `text` instead of inferring the
element type from the values.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run("CREATE TABLE iv1 (i int, g int, d interval)")
    run("CREATE TABLE iv2 (i int, d interval)")
    run(
        "INSERT INTO iv1 VALUES (1,1,interval '1 day'), (2,1,interval '2 day'),"
        " (3,2,interval '3 hours'), (4,2,NULL), (5,3,interval '1 mon 2 days 3:04:05')"
    )
    run("INSERT INTO iv2 VALUES (1,interval '10 min'),(2,interval '20 min')")
    try:
        yield run
    finally:
        storage.close()


def _text(db, sql):
    return [r[0] for r in db(sql)]


class TestIntervalSumAndAvg:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT sum(d)::text FROM iv1", "1 mon 5 days 06:04:05"),
            ("SELECT avg(d)::text FROM iv1", "8 days 19:31:01.25"),
            ("SELECT sum(d)::text FROM iv2", "00:30:00"),
            ("SELECT avg(d)::text FROM iv2", "00:15:00"),
        ],
    )
    def test_whole_table(self, db, sql, want):
        assert _text(db, sql) == [want]

    @pytest.mark.parametrize(
        ("agg", "want"),
        [
            ("sum", ["3 days", "03:00:00", "1 mon 2 days 03:04:05"]),
            ("avg", ["1 day 12:00:00", "03:00:00", "1 mon 2 days 03:04:05"]),
        ],
    )
    def test_grouped(self, db, agg, want):
        rows = db(f"SELECT g, {agg}(d)::text FROM iv1 GROUP BY g ORDER BY g")
        assert [r[1] for r in rows] == want

    @pytest.mark.parametrize(
        "sql",
        [
            "SELECT sum(d)::text FROM iv1 WHERE i > 100",
            "SELECT sum(d)::text FROM iv1 WHERE d IS NULL",
            "SELECT avg(d)::text FROM iv1 WHERE i > 100",
        ],
    )
    def test_no_contributing_rows_is_null(self, db, sql):
        """Zero non-NULL values is NULL for every SQL aggregate — and this is
        also what a plain `$sum` of nothing would have answered 0 for."""
        assert _text(db, sql) == [None]

    def test_nulls_are_skipped(self, db):
        """Group 2 has one interval and one NULL: the average is the one value,
        not half of it."""
        rows = db("SELECT g, avg(d)::text FROM iv1 WHERE g = 2 GROUP BY g")
        assert rows == [(2, "03:00:00")]

    def test_min_max_unaffected(self, db):
        assert db("SELECT min(d)::text, max(d)::text FROM iv1") == [
            ("03:00:00", "1 mon 2 days 03:04:05")
        ]

    def test_count_unaffected(self, db):
        assert db("SELECT count(d) FROM iv1") == [(4,)]


class TestNonColumnArguments:
    """The interval check asks for the argument's declared type, and the
    resolver RAISES for anything that is not a column. `SUM(-83)` takes a
    literal, so an unguarded check turned a working query into
    `expected a column: -83`."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT sum(-83) FROM iv2", -166),
            ("SELECT sum(i * 2) FROM iv2", 6),
            ("SELECT avg(4) FROM iv2", 4),
        ],
    )
    def test_literal_and_computed_arguments(self, db, sql, want):
        assert db(sql)[0][0] == want

    def test_literal_argument_across_a_join(self, db):
        assert db("SELECT sum(-83) FROM iv1 JOIN iv2 ON iv1.i=iv2.i")[0][0] == -166


class TestJoinPath:
    def test_join(self, db):
        assert _text(db, "SELECT sum(iv1.d)::text FROM iv1 JOIN iv2 ON iv1.i=iv2.i") == ["3 days"]

    @pytest.mark.parametrize(("agg", "want"), [("sum", "00:30:00"), ("avg", "00:15:00")])
    def test_join_grouped(self, db, agg, want):
        rows = db(
            f"SELECT iv1.g, {agg}(iv2.d)::text FROM iv1 JOIN iv2"
            " ON iv1.i=iv2.i GROUP BY iv1.g ORDER BY iv1.g"
        )
        assert rows == [(1, want)]


class TestComposesWithArithmetic:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT (sum(d) + interval '1 hour')::text FROM iv2", "01:30:00"),
            ("SELECT (avg(d) * 2)::text FROM iv2", "00:30:00"),
        ],
    )
    def test_result_is_a_real_interval(self, db, sql, want):
        assert _text(db, sql) == [want]


class TestIntervalInsideAnArray:
    """The text cast defaulted every array element to `text`, so an interval
    element rendered as our internal subdocument."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT ARRAY[interval '1 day', interval '2 hours']::text", '{"1 day",02:00:00}'),
            ("SELECT array_agg(d)::text FROM iv2", "{00:10:00,00:20:00}"),
            ("SELECT array_agg(d ORDER BY d DESC)::text FROM iv2", "{00:20:00,00:10:00}"),
            # A single element was always fine — it never went through the
            # array renderer.
            ("SELECT (ARRAY[interval '1 day'])[1]::text", "1 day"),
        ],
    )
    def test_renders_as_intervals(self, db, sql, want):
        assert _text(db, sql) == [want]

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT ARRAY['a','b']::text", "{a,b}"),
            ("SELECT ARRAY[1,2]::text", "{1,2}"),
            ("SELECT array_agg(i)::text FROM iv2", "{1,2}"),
        ],
    )
    def test_other_element_types_unchanged(self, db, sql, want):
        assert _text(db, sql) == [want]
