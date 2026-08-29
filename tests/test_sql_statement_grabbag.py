"""pgjdbc StatementTest / PreparedStatementTest conformance fixes, pinned.

The clusters this file covers (each one a pgjdbc gauge failure before the fix):

* dollar-quoted string literals in any expression position (sqlglot tokenizes
  them as ``RawString``; the planner normalizes to ``Literal``), including
  digit-bearing tags (``$A0$``) and tag-vs-content ambiguity;
* nested block comments (PG nests ``/* /* */ */``; sqlglot does not — the
  planner strips them pre-parse when they nest);
* ``standard_conforming_strings = off`` backslash escapes in plain literals;
* the scalar functions behind pgjdbc's ``{fn …}`` escapes (trig, ``replace``,
  numeric-aware ``power`` / ``trunc``);
* transaction-stable ``now()`` / ``CURRENT_TIMESTAMP``;
* ``CREATE [TEMP] TABLE … AS SELECT`` with PG's ``SELECT <n>`` tag;
* ``TRUNCATE`` resolving schema-qualified and session-temp names;
* ``to_char`` word tokens (``Day`` / ``Dy`` / ``Month`` / ``Mon``).
"""

from __future__ import annotations

import math

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def one(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


class TestDollarQuotedStrings:
    # The exact queries pgjdbc's StatementTest.parsingDollarQuotes /
    # PreparedStatementTest.testDollarQuotes send.
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT '$a$ ; $a$'", "$a$ ; $a$"),
            ("SELECT $$;$$", ";"),
            ("SELECT $B$;$b$B$", ";$b"),
            ("SELECT $c$c$;$c$", "c$;"),
            ("SELECT $A0$;$A0$ WHERE ''=$t$t$t$ OR ';$t$'=';$t$'", ";"),
            (
                "SELECT $OR$$a$'$b$a$$OR$ WHERE '$a$''$b$a$'=$OR$$a$'$b$a$$OR$OR ';'=''",
                "$a$'$b$a$",
            ),
            ("SELECT /* */$$;$$/**//*;*/", ";"),
            ("SELECT /* */--;\n$$a$$/**/--\n--;\n", "a"),
        ],
    )
    def test_literal(self, storage, session, sql, want):
        assert one(storage, session, sql) == want

    def test_large_body_roundtrips(self, storage, session):
        body = "  var _modules = {};\n  var _current_stack = [];\n"
        assert one(storage, session, f"select $JAVASCRIPT${body}$JAVASCRIPT$") == body

    def test_dollar_in_identifier_not_a_quote(self, storage, session):
        run(storage, session, "CREATE TABLE a$b$c (a text)")
        run(storage, session, "INSERT INTO a$b$c VALUES ('x')")
        assert one(storage, session, "SELECT a FROM a$b$c") == "x"


class TestNestedBlockComments:
    def test_nested_comment_soup(self, storage, session):
        # PreparedStatementTest.testComments' first statement shape.
        assert one(storage, session, "SELECT /*?*/ /*/*/*/**/*/*/*/1") == 1

    def test_nested_comment_before_literal(self, storage, session):
        assert one(storage, session, "SELECT /**/'?'/*/**/*/ WHERE '?'='?'") == "?"

    def test_simple_comments_untouched(self, storage, session):
        assert one(storage, session, "SELECT /* c */ 2 -- t\n") == 2


class TestNonstandardStrings:
    def test_backslash_escapes_when_off(self, storage, session):
        run(storage, session, "SET standard_conforming_strings TO off")
        assert one(storage, session, r"SELECT 'quoted \' single quote'") == "quoted ' single quote"
        assert one(storage, session, r"SELECT 'octal \060 constant'") == "octal 0 constant"
        assert one(storage, session, r"SELECT 'double \\ backslash'") == "double \\ backslash"
        assert one(storage, session, "SELECT 'doubled '' single quote'") == "doubled ' single quote"

    def test_standard_mode_keeps_backslashes(self, storage, session):
        assert one(storage, session, r"SELECT 'a\b'") == r"a\b"


class TestEscapeFunctions:
    # The native calls pgjdbc's {fn …} escapes rewrite to.
    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("select acos(-0.6)", math.acos(-0.6)),
            ("select asin(-0.6)", math.asin(-0.6)),
            ("select atan(-0.6)", math.atan(-0.6)),
            ("select atan2(-2.3,7)", math.atan2(-2.3, 7)),
            ("select cos(-2.3)", math.cos(-2.3)),
            ("select cot(-2.3)", 1 / math.tan(-2.3)),
            ("select sin(-2.3)", math.sin(-2.3)),
            ("select tan(-2.3)", math.tan(-2.3)),
            ("select power(7,-2.3)", 7**-2.3),
            ("select trunc(3.1294::numeric,2)", 3.12),
            ("select round(3.1294::numeric,2)", 3.13),
        ],
    )
    def test_numeric(self, storage, session, sql, want):
        assert float(one(storage, session, sql)) == pytest.approx(float(want))

    def test_replace(self, storage, session):
        assert one(storage, session, "select replace('abcdbc','bc','x')") == "axdx"

    def test_replace_null_propagates(self, storage, session):
        assert one(storage, session, "select replace('a',NULL,'x')") is None

    def test_round_trunc_on_bare_literals(self, storage, session):
        # In a multi-column select these literals evaluate to Decimal128 —
        # round()/trunc() crashed with an internal error before the unwrap.
        row = run(storage, session, "select pi(), round(3.1294,2), trunc(3.1294,2)").rows[0]
        assert float(row[1]) == 3.13
        assert float(row[2]) == 3.12

    def test_round_is_half_away_from_zero(self, storage, session):
        # PG numeric rounding, not Python banker's rounding.
        assert float(one(storage, session, "select round(2.5)")) == 3.0
        assert float(one(storage, session, "select round(3.125, 2)")) == 3.13


class TestStableNow:
    def test_same_instant_within_statement(self, storage, session):
        row = run(storage, session, "select now(), now(), current_timestamp").rows[0]
        assert row[0] == row[1] == row[2]

    def test_advances_between_statements(self, storage, session):
        a = one(storage, session, "select now()")
        b = one(storage, session, "select now()")
        assert b >= a  # distinct statements re-derive the clock

    def test_frozen_across_transaction(self, storage, session):
        import time as _time

        run(storage, session, "BEGIN")
        a = one(storage, session, "select now()")
        b = one(storage, session, "select now()")
        run(storage, session, "COMMIT")
        assert a == b
        # Outrun Windows py3.10's ~15.6 ms system-clock tick — two
        # transactions inside one tick read identical now() values, which
        # this assertion would misread as "frozen across transactions".
        _time.sleep(0.02)
        assert one(storage, session, "select now()") != a

    def test_interval_roundtrip_is_exact(self, storage, session):
        # StatementTest.dateFunctions' timestampdiff shape: 3s must extract as
        # exactly 3, which needs both now() calls to be the same instant.
        got = one(
            storage,
            session,
            "select extract( second from ((CAST(3||' second' as interval)+now())-now()))",
        )
        assert float(got) == 3.0


class TestCreateTableAs:
    def test_ctas_tag_is_select_n(self, storage, session):
        res = run(storage, session, "CREATE TABLE yat AS SELECT x FROM generate_series(1,10) x")
        assert res.command_tag == "SELECT 10"
        assert one(storage, session, "SELECT count(*) FROM yat") == 10

    def test_temp_ctas(self, storage, session):
        res = run(storage, session, "CREATE TEMP TABLE tt AS SELECT 1 AS a, 'b' AS b")
        assert res.command_tag == "SELECT 1"
        assert run(storage, session, "SELECT a, b FROM tt").rows == [(1, "b")]

    def test_if_not_exists_skips(self, storage, session):
        run(storage, session, "CREATE TABLE t2 AS SELECT 1 AS a")
        res = run(storage, session, "CREATE TABLE IF NOT EXISTS t2 AS SELECT 99 AS zz")
        assert res.command_tag == "CREATE TABLE AS"
        assert one(storage, session, "SELECT a FROM t2") == 1

    def test_empty_source(self, storage, session):
        run(storage, session, "CREATE TABLE src (a int)")
        res = run(storage, session, "CREATE TABLE dst AS SELECT * FROM src WHERE a > 5")
        assert res.command_tag == "SELECT 0"
        assert one(storage, session, "SELECT count(*) FROM dst") == 0

    def test_created_columns_are_typed(self, storage, session):
        run(storage, session, "CREATE TABLE tt2 AS SELECT 1 AS n, 'x' AS s, now() AS ts")
        assert one(storage, session, "SELECT n + 1 FROM tt2") == 2


class TestTruncateQualified:
    def test_truncate_temp_table(self, storage, session):
        run(storage, session, "CREATE TEMP TABLE decimal_scale (n1 numeric)")
        run(storage, session, "INSERT INTO decimal_scale VALUES (1)")
        run(storage, session, "TRUNCATE TABLE decimal_scale")
        assert one(storage, session, "SELECT count(*) FROM decimal_scale") == 0

    def test_truncate_schema_qualified(self, storage, session):
        run(storage, session, "CREATE SCHEMA s1")
        run(storage, session, "CREATE TABLE s1.tt (a int)")
        run(storage, session, "INSERT INTO s1.tt VALUES (1)")
        run(storage, session, "TRUNCATE TABLE s1.tt")
        assert one(storage, session, "SELECT count(*) FROM s1.tt") == 0


class TestToCharWordTokens:
    @pytest.mark.parametrize(
        ("fmt", "want"),
        [
            ("Day", "Monday"),
            ("Dy", "Mon"),
            ("Month", "January"),
            ("Mon", "Jan"),
            ("YYYY-MM-DD", "2005-01-17"),
            ("Day DD, Month YYYY", "Monday 17, January 2005"),
        ],
    )
    def test_word_tokens(self, storage, session, fmt, want):
        got = one(storage, session, f"select to_char(timestamp '2005-01-17 12:00:00','{fmt}')")
        assert got == want
