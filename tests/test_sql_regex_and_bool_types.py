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


class TestLikeDanglingEscape:
    """A LIKE pattern ending in an UNESCAPED escape character matches nothing.

    The trailing escape has no character to escape. Falling through to the
    default branch treated it as a literal backslash, so a pattern of
    a-backslash matched a value of a-backslash where PostgreSQL returns no rows.

    Measured on PostgreSQL 14 (2026-09-25) with a matching row present and the
    pattern bound as a parameter: no rows, no error. A dangling escape written
    as a LITERAL in the SQL text is instead a plan-time error there ("LIKE
    pattern must not end with escape character"); we do not distinguish the two
    paths and implement the bound semantics, which is what a client relies on --
    pgjdbc's getTables binds the pattern. Recorded in tasks/backlog.md.

    Every case builds its SQL with chr(92) rather than a source-level escape.
    Writing these as literals silently lost backslashes across the Python-source
    and SQL layers -- pytest reported running `'a' LIKE 'a\'` for a case
    written to be a-backslash against a doubled one -- so the cases tested
    something other than what they claimed.

    Found from pgjdbc's DatabaseMetaDataTest::escaping.
    """

    BS = chr(92)

    @pytest.mark.parametrize(
        ("value_sql", "pattern_sql", "escape_sql", "expected", "why"),
        [
            # A dangling escape matches nothing, even a value that a literal
            # reading of the backslash WOULD match.
            ("a" + BS, "a" + BS, "", False, "dangling escape matches nothing"),
            # An ESCAPED escape is not dangling: the first consumes the second,
            # so it matches one literal backslash. A first version of the guard
            # used `endswith` and broke exactly this.
            ("a" + BS, "a" + BS + BS, "", True, "escaped escape matches a literal"),
            # The same rule under a custom escape character.
            ("a#", "a#", "#", False, "dangling custom escape matches nothing"),
            ("a#", "a##", "#", True, "escaped custom escape matches a literal"),
            # With escaping DISABLED a trailing backslash is just a character.
            ("a" + BS, "a" + BS, "DISABLED", True, "ESCAPE '' makes it literal"),
        ],
    )
    def test_dangling(self, db, value_sql, pattern_sql, escape_sql, expected, why):
        def lit(text: str) -> str:
            return "'" + text.replace("'", "''") + "'"

        sql = f"SELECT {lit(value_sql)} LIKE {lit(pattern_sql)}"
        if escape_sql == "DISABLED":
            sql += " ESCAPE ''"
        elif escape_sql:
            sql += f" ESCAPE {lit(escape_sql)}"
        assert db(sql)[0] == [(expected,)], why


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
