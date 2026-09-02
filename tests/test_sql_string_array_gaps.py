"""What a broad differential sweep against PostgreSQL 14.13 turned up.

Nine divergences across string, array and date builtins, found by running a
corpus of ordinary SQL against both servers and diffing — not by reading the
backlog. Three of them were **silently wrong answers** rather than errors:

- `trim(both 'x' from 'xxabxx')` answered `'xxabxx'`. The trim characters and
  the position were both ignored; every spelling ran a plain `str.strip()`.
- `substr('abcdef', -1, 3)` answered `'abc'`. The start was clamped to 1 and
  the length counted from there; PostgreSQL counts from the ORIGINAL start, so
  positions -1, 0 and 1 leave just `'a'`.
- `unnest('{1,2,3}'::int[])` handed the driver `'1'`, `'2'`, `'3'` — the
  elements were typed `any` and went out as text.

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

    run("CREATE TABLE ar (i int, a int[], t text)")
    run("INSERT INTO ar VALUES (1,'{1,2}','x'), (2,NULL,NULL)")
    try:
        yield run
    finally:
        storage.close()


def _one(db, sql):
    rows, _ = db(sql)
    return rows[0][0]


class TestTrim:
    """The trim CHARACTERS and the POSITION were both ignored."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT trim(both 'x' from 'xxabxx')", "ab"),
            ("SELECT trim(leading 'x' from 'xxabxx')", "abxx"),
            ("SELECT trim(trailing 'x' from 'xxabxx')", "xxab"),
            # The second operand is a SET of characters, not a substring.
            ("SELECT trim(both 'xy' from 'yxabxy')", "ab"),
            ("SELECT trim(both '' from 'abc')", "abc"),
            # No character set still means whitespace.
            ("SELECT trim('  ab  ')", "ab"),
            ("SELECT trim(from '  ab  ')", "ab"),
        ],
    )
    def test_trim(self, db, sql, want):
        assert _one(db, sql) == want

    def test_null_operands(self, db):
        assert _one(db, "SELECT trim(both 'x' from NULL)") is None
        assert _one(db, "SELECT trim(both NULL from 'abc')") is None

    def test_btrim_family_was_always_right(self, db):
        """They take the characters as an ordinary second argument, so they
        never went through the broken path."""
        assert _one(db, "SELECT btrim('xxabxx','x')") == "ab"


class TestSubstr:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            # The length is counted from the ORIGINAL start, then clipped.
            ("SELECT substr('abcdef',-1,3)", "a"),
            ("SELECT substr('abcdef',0,3)", "ab"),
            ("SELECT substr('abcdef',-5,3)", ""),
            ("SELECT substr('abcdef',2,0)", ""),
            # Unchanged shapes.
            ("SELECT substr('abcdef',2,3)", "bcd"),
            ("SELECT substr('abcdef',2)", "bcdef"),
            ("SELECT substr('abcdef',-1)", "abcdef"),
        ],
    )
    def test_substr(self, db, sql, want):
        assert _one(db, sql) == want

    def test_negative_length(self, db):
        with pytest.raises(SQLError) as exc:
            db("SELECT substr('abcdef',2,-1)")
        assert exc.value.sqlstate == "22011"
        assert exc.value.message == "negative substring length not allowed"


class TestArrayConcatNull:
    """A NULL ARRAY is empty in a concatenation; a NULL of any other type makes
    the whole `||` NULL. Both are `None` here, so the array-ness comes from the
    node — a cast and an `ARRAY[…]` are read directly, a column is stamped by
    the type-checking pass."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT (NULL::int[] || 9)::text", "{9}"),
            ("SELECT (9 || NULL::int[])::text", "{9}"),
            ("SELECT ('{1}'::int[] || NULL::int[])::text", "{1}"),
            # A NULL ELEMENT stays a NULL element.
            ("SELECT (NULL::int || '{1}'::int[])::text", "{NULL,1}"),
            ("SELECT (ARRAY[1,2] || NULL::int)::text", "{1,2,NULL}"),
        ],
    )
    def test_static_nodes(self, db, sql, want):
        assert _one(db, sql) == want

    def test_both_null_arrays_is_null(self, db):
        assert _one(db, "SELECT NULL::int[] || NULL::int[]") is None

    def test_null_array_column(self, db):
        rows, _ = db("SELECT i, (a || 9)::text, (0 || a)::text FROM ar ORDER BY i")
        assert rows == [(1, "{1,2,9}", "{0,1,2}"), (2, "{9}", "{0}")]

    def test_text_concat_is_unaffected(self, db):
        assert _one(db, "SELECT NULL::text || 'y'") is None
        rows, _ = db("SELECT t || 'y' FROM ar ORDER BY i")
        assert rows == [("xy",), (None,)]


class TestUnnestElementType:
    @pytest.mark.parametrize(
        ("sql", "tag"),
        [
            ("SELECT unnest('{1,2}'::int[])", "int4"),
            ("SELECT unnest('{1,2}'::bigint[])", "int8"),
            # An ARRAY[…] of integer literals is `integer` unless one does not
            # fit — Postgres' own literal rule.
            ("SELECT unnest(ARRAY[1,2])", "int4"),
            ("SELECT unnest(ARRAY[3000000000])", "int8"),
            ("SELECT unnest(ARRAY[1.5])", "numeric"),
            ("SELECT unnest(ARRAY['a'])", "text"),
            ("SELECT unnest(ARRAY[true])", "bool"),
        ],
    )
    def test_element_tag(self, db, sql, tag):
        _rows, tags = db(sql)
        assert tags[0] == tag

    def test_values_are_not_text(self, db):
        rows, _ = db("SELECT unnest('{1,2,3}'::int[])")
        assert rows == [(1,), (2,), (3,)]


class TestNewBuiltins:
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT to_hex(255)", "ff"),
            ("SELECT to_hex(0)", "0"),
            # A negative value is its unsigned two's complement, at the
            # argument's OWN width.
            ("SELECT to_hex(-1)", "ffffffff"),
            ("SELECT to_hex((-1)::bigint)", "ffffffffffffffff"),
            ("SELECT to_hex(9223372036854775807::bigint)", "7fffffffffffffff"),
        ],
    )
    def test_to_hex(self, db, sql, want):
        assert _one(db, sql) == want

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT make_date(2020,2,29)::text", "2020-02-29"),
            ("SELECT make_time(10,30,45.5)::text", "10:30:45.5"),
            ("SELECT make_time(10,30,45)::text", "10:30:45"),
            ("SELECT make_timestamp(2020,1,2,3,4,5.5)::text", "2020-01-02 03:04:05.5"),
        ],
    )
    def test_make_functions(self, db, sql, want):
        # `str()` because a `date::text` comes back in-process as the `date`
        # itself and is rendered on the way out; over the wire both spellings
        # give the same text, which is what the probe against PG compared.
        assert str(_one(db, sql)) == want

    @pytest.mark.parametrize(
        ("sql", "message"),
        [
            ("SELECT make_date(2020,2,30)", "date field value out of range: 2020-02-30"),
            ("SELECT make_time(25,0,0)", "time field value out of range: 25:00:00"),
        ],
    )
    def test_out_of_range(self, db, sql, message):
        with pytest.raises(SQLError) as exc:
            db(sql)
        assert exc.value.sqlstate == "22008"
        assert exc.value.message == message

    def test_null_argument(self, db):
        assert _one(db, "SELECT make_date(2020,1,NULL)") is None
