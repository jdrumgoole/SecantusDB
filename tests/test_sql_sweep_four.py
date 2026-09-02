"""What a fourth sweep — windows, aggregates, bytea, subqueries — turned up.

25 of 35 shapes matched PostgreSQL 14.13. Two of the misses were badly wrong
rather than merely refused:

* `substring(b from 1 for 1)` over a `bytea` answered the string `'b'` — the
  first character of the Python repr `b'\\x01\\x02'` — where PostgreSQL answers
  the byte `\\x01`.
* `every(n > 5)` answered NULL. `every` is the standard-SQL spelling of
  `bool_and`, and the mapping kept the name but dropped an EXPRESSION argument;
  `bool_and(n > 5)`, the same aggregate, was right all along.

And three wrong types: a scalar subquery came back as `text` (so
`(SELECT count(*) FROM t)` sent the string `'3'` under oid 25), `round()` over a
float claimed `numeric`, and a `::numeric(p, s)` CAST did not round to its
declared scale the way the column path does.

`GROUPS` window frames were not handled at all — they fell through to the RANGE
branch and reported an error about a clause the user had not written.

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
        res = [r for r in run_sql(storage, "t", sql, session=session)][0]
        return res.rows, [c.type_tag for c in res.columns]

    run("CREATE TABLE w1 (id int, g text, n int, b bytea)")
    run("INSERT INTO w1 VALUES (1,'a',10,E'\\\\x0102'), (2,'a',20,E'\\\\x0203'), (3,'b',30,NULL)")
    run("CREATE TABLE gw (id int, g text)")
    run("INSERT INTO gw VALUES (1,'a'),(2,'a'),(3,'b'),(4,'c'),(5,'c')")
    try:
        yield run
    finally:
        storage.close()


def _one(db, sql):
    rows, _ = db(sql)
    return rows[0][0]


def _tag(db, sql):
    _rows, tags = db(sql)
    return tags[0]


class TestByteaSubstring:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT substring(E'\\\\x0102'::bytea from 1 for 1)", b"\x01"),
            ("SELECT substring(E'\\\\x0102'::bytea from 2)", b"\x02"),
            ("SELECT substr(E'\\\\x0102'::bytea, 1, 1)", b"\x01"),
        ],
    )
    def test_slices_bytes(self, db, sql, want):
        assert _one(db, sql) == want

    def test_result_is_bytea(self, db):
        assert _tag(db, "SELECT substring(b from 1 for 1) FROM w1 WHERE id = 1") == "bytea"

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT substring('abcdef' from 2 for 3)", "bcd"),
            # The POSIX-regex form is text-only and must not be caught by the
            # bytes branch.
            ("SELECT substring('abc123' from '[0-9]+')", "123"),
        ],
    )
    def test_text_is_unaffected(self, db, sql, want):
        assert _one(db, sql) == want


class TestEvery:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT every(n > 5) FROM w1", True),
            ("SELECT every(n > 25) FROM w1", False),
            # The same aggregate under its other name was always right.
            ("SELECT bool_and(n > 5) FROM w1", True),
            ("SELECT bool_or(n > 25) FROM w1", True),
        ],
    )
    def test_expression_argument(self, db, sql, want):
        assert _one(db, sql) is want


class TestScalarSubqueryType:
    @pytest.mark.parametrize(
        ("sql", "tag"),
        [
            ("SELECT (SELECT count(*) FROM w1)", "int8"),
            ("SELECT id, (SELECT count(*) FROM w1 b WHERE b.g=w1.g) FROM w1", "int4"),
        ],
    )
    def test_tags(self, db, sql, tag):
        _rows, tags = db(sql)
        assert tags[0] == tag

    def test_correlated_aggregate_takes_its_column_type(self, db):
        _rows, tags = db("SELECT id, (SELECT max(n) FROM w1 b WHERE b.g=w1.g) FROM w1")
        assert tags == ["int4", "int4"]

    def test_values_unchanged(self, db):
        rows, _ = db("SELECT id, (SELECT count(*) FROM w1 b WHERE b.g=w1.g) FROM w1 ORDER BY id")
        assert rows == [(1, 2), (2, 2), (3, 1)]


class TestNumericCastTypmod:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT (10::numeric(5,2))::text", "10.00"),
            ("SELECT (10.567::numeric(5,2))::text", "10.57"),
            # An unconstrained numeric keeps its own scale.
            ("SELECT (10::numeric)::text", "10"),
            ("SELECT (1.5::numeric)::text", "1.5"),
        ],
    )
    def test_cast_rounds(self, db, sql, want):
        assert _one(db, sql) == want

    def test_cast_overflow(self, db):
        with pytest.raises(SQLError) as exc:
            db("SELECT 99999::numeric(5,2)")
        assert exc.value.sqlstate == "22003"


class TestRoundingTypes:
    @pytest.mark.parametrize(
        ("sql", "tag"),
        [
            ("SELECT round(2.345::float8)", "float8"),
            ("SELECT round(2.345::numeric, 2)", "numeric"),
            ("SELECT floor(2.9::float8)", "float8"),
            ("SELECT ceil(2.1::numeric)", "numeric"),
        ],
    )
    def test_tag_follows_the_argument(self, db, sql, tag):
        assert _tag(db, sql) == tag


class TestGroupsFrame:
    """The offset counts PEER GROUPS, not rows. `GROUPS` fell through to the
    RANGE branch and reported an error about a clause the user had not
    written."""

    @pytest.mark.parametrize(
        ("frame", "want"),
        [
            ("GROUPS BETWEEN CURRENT ROW AND 1 FOLLOWING", [3, 3, 3, 2, 2]),
            ("GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW", [2, 2, 3, 3, 3]),
            ("GROUPS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW", [2, 2, 3, 5, 5]),
            ("GROUPS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING", [5, 5, 3, 2, 2]),
            ("GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING", [3, 3, 5, 3, 3]),
            ("GROUPS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING", [5, 5, 5, 5, 5]),
            ("GROUPS BETWEEN CURRENT ROW AND CURRENT ROW", [2, 2, 1, 2, 2]),
        ],
    )
    def test_frames(self, db, frame, want):
        rows, _ = db(f"SELECT id, count(*) OVER (ORDER BY g {frame}) FROM gw ORDER BY id")
        assert [r[1] for r in rows] == want
