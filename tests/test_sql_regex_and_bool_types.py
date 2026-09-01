"""`substring(x FROM regex)`, LIKE's default escape, and three more wrong types.

Found 2026-09-01 by a sweep over pattern matching, ordering, arrays and
subqueries. Four distinct failures, in descending severity:

* **`substring('abc123' FROM '[0-9]+')` reached the wire as `XX000 internal
  error`.** sqlglot parks the POSIX pattern in the same `start` slot as the
  positional form, so `int('[0-9]+')` raised `ValueError`. This project treats
  a leaked internal error as never acceptable.
* **`'a_c' LIKE 'a\\_c'` was FALSE** where PostgreSQL says true. Backslash is
  PostgreSQL's DEFAULT escape for LIKE; `_like_to_regex` only escaped when an
  explicit `ESCAPE` clause was given, and collapsed "unset" with `ESCAPE ''`
  (which genuinely disables escaping).
* **`BETWEEN`, `EXISTS` and a scalar subquery all reported `text`** — 't'/'f'
  and the string '1' on the wire, where PostgreSQL sends bool and int4.
* Four array/regex functions were absent.
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
        res = [r for r in run_sql(storage, "t", sql, session=session)][0]
        return res.rows, [c.type_tag for c in res.columns]

    try:
        yield run
    finally:
        storage.close()


class TestSubstringFromPattern:
    @pytest.mark.parametrize(
        ("expr", "value"),
        [
            ("substring('abc123' from '[0-9]+')", "123"),
            ("substring('abc123' from '([a-z]+)')", "abc"),
            ("substring('abc123' from '([a-z]+)([0-9]+)')", "abc"),
            ("substring('abc' from '[0-9]+')", None),
            # The positional forms must keep working.
            ("substring('abcdef' from 2 for 3)", "bcd"),
            ("substring('abcdef' from 3)", "cdef"),
        ],
    )
    def test_forms(self, db, expr, value):
        assert db(f"SELECT {expr}")[0] == [(value,)]

    def test_it_is_not_an_internal_error(self, db):
        """It answered `XX000 internal error` — a Python ValueError on the
        wire."""
        rows, _ = db("SELECT substring('abc123' from '[0-9]+')")
        assert rows == [("123",)]


class TestLikeDefaultEscape:
    @pytest.mark.parametrize(
        ("expr", "value"),
        [
            (r"'a_c' LIKE 'a\_c'", True),
            (r"'axc' LIKE 'a\_c'", False),
            (r"'a%c' LIKE 'a\%c'", True),
            (r"'abc' LIKE 'a\%c'", False),
            # A custom escape still wins.
            ("'a_c' LIKE 'a#_c' ESCAPE '#'", True),
            # `ESCAPE ''` DISABLES escaping — the case that must not collapse
            # into the default.
            (r"'axc' LIKE 'a\_c' ESCAPE ''", False),
            # Plain wildcards are unaffected.
            ("'axc' LIKE 'a_c'", True),
            ("'abc' LIKE 'a%'", True),
        ],
    )
    def test_escape(self, db, expr, value):
        assert db(f"SELECT {expr}")[0] == [(value,)]


class TestBooleanAndSubqueryTypes:
    @pytest.mark.parametrize(
        "expr", ["1 BETWEEN 0 AND 2", "1 NOT BETWEEN 5 AND 9", "EXISTS (SELECT 1 WHERE false)"]
    )
    def test_boolean_expressions_are_bool(self, db, expr):
        rows, tags = db(f"SELECT {expr}")
        assert tags == ["bool"]
        assert isinstance(rows[0][0], bool)

    def test_scalar_subquery_takes_its_projections_type(self, db):
        rows, tags = db("SELECT (SELECT 1)")
        assert rows == [(1,)]
        assert tags == ["int4"]

    def test_scalar_subquery_of_text_is_text(self, db):
        rows, tags = db("SELECT (SELECT 'x')")
        assert rows == [("x",)]
        assert tags == ["text"]


class TestArrayAndRegexBuiltins:
    @pytest.mark.parametrize(
        ("expr", "value"),
        [
            ("regexp_match('abc123','([a-z]+)([0-9]+)')", ["abc", "123"]),
            ("regexp_match('abc123','[a-z]+')", ["abc"]),
            ("regexp_match('abc','[0-9]+')", None),
            ("regexp_split_to_array('a,b,c', ',')", ["a", "b", "c"]),
            ("string_to_array('a,b', ',')", ["a", "b"]),
            ("array_replace(ARRAY[1,2,1], 1, 9)", [9, 2, 9]),
        ],
    )
    def test_values(self, db, expr, value):
        assert db(f"SELECT {expr}")[0] == [(value,)]

    def test_array_replace_keeps_the_arrays_type(self, db):
        """A fixed text tag rendered the array as its literal `{1,9}` text."""
        _rows, tags = db("SELECT array_replace(ARRAY[1,2], 2, 9)")
        assert tags == ["int4[]"]
