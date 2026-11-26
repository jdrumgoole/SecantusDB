"""What working RP-423 (a regex estimate over a back reference) found:

- a back reference was an invalid regex outside a WHERE: the scalar
  evaluator and the regex builtins compiled with a linear engine that
  refuses it (RP-1022, fixed: they fall back to a backtracking one);
- a back reference under `~*` compared case-sensitively (RP-1022, fixed:
  fancy-regex 0.20);
- a director or an embedded option was an invalid regex, and `.` did not
  match a newline (RP-1122, 2026-11-26);
- an invalid regex was 22P02 with the `regex` crate's message, or accepted
  (RP-1123, 2026-11-26).

An answer is the rows and the column names, or `(sqlstate, message,
position)` for an error. Measured on PostgreSQL 15.19 (the shared reference
on port 5415; RP-1122 and RP-1123 on 15.20, its upgrade): every statement
is VALUES-only.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> str:
    return _dsn(rust_pg) + " dbname=postgres"


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return (cur.fetchall(), [d.name for d in cur.description or []])


BACK_REFERENCES = {
    r"select 'abab' ~ '(ab)\1' as m": ([(True,)], ["m"]),
    r"select regexp_replace('abab', '(ab)\1', 'X') as r": ([("X",)], ["r"]),
    r"select regexp_match('xabab', '(ab)\1') as r": ([(["ab"],)], ["r"]),
    r"select 'abac' ~ '(ab)\1' as m": ([(False,)], ["m"]),
    r"select regexp_replace('ababxcdcd', '(\w\w)\1', '[\1]', 'g') as r": (
        [("[ab]x[cd]",)],
        ["r"],
    ),
    r"select regexp_matches('ababcdcd', '(\w\w)\1', 'g') as r": (
        [(["ab"],), (["cd"],)],
        ["r"],
    ),
    r"select regexp_count('ababab abab', '(ab)\1') as n": ([(2,)], ["n"]),
    r"select regexp_instr('xxabab', '(ab)\1') as n": ([(3,)], ["n"]),
    r"select regexp_substr('xxabab', '(ab)\1') as r": ([("abab",)], ["r"]),
    r"select regexp_replace('ababab', '(ab)\1', 'Y', 2) as r": ([("abY",)], ["r"]),
    r"select substring('xabab' from '(ab)\1') as r": ([("ab",)], ["r"]),
}


@pytest.mark.backlog_fixed("RP-1022")
@pytest.mark.parametrize("query", list(BACK_REFERENCES))
def test_a_back_reference_outside_a_where(query: str, dsn: str) -> None:
    assert answer(dsn, query) == BACK_REFERENCES[query]


# fancy-regex 0.14 compared a back reference byte for byte even under
# `(?i)`; 0.20 folds it. The WHERE path (`secantus-core`) runs the same
# engine. ASCII only: the reference's database is `ctype C`, which folds no
# other letter.
CASE_INSENSITIVE = {
    r"select 'ABab' ~* '(ab)\1' as m": ([(True,)], ["m"]),
    r"select 'ABab' ~ '(?i)(ab)\1' as m": ([(True,)], ["m"]),
    r"select 'abAB' !~* '(ab)\1' as m": ([(False,)], ["m"]),
    r"select 'ABab' ~ '(ab)\1' as m": ([(False,)], ["m"]),
    r"select regexp_replace('ABabX', '(ab)\1', 'Y', 'i') as r": ([("YX",)], ["r"]),
    r"select regexp_match('xAbaB', '(ab)\1', 'i') as r": ([(["Ab"],)], ["r"]),
    r"select regexp_count('ABab abAB', '(ab)\1', 1, 'i') as n": ([(2,)], ["n"]),
    r"select x from (values ('ABab'), ('abab'), ('abAB'), ('abcd')) v(x)"
    r" where x ~* '(ab)\1' order by x": ([("ABab",), ("abAB",), ("abab",)], ["x"]),
}


@pytest.mark.backlog_fixed("RP-1022")
@pytest.mark.parametrize("query", list(CASE_INSENSITIVE))
def test_a_case_insensitive_back_reference(query: str, dsn: str) -> None:
    """Under `~*` the back reference matched case-sensitively."""
    assert answer(dsn, query) == CASE_INSENSITIVE[query]


@pytest.mark.backlog_fixed("RP-1022")
def test_a_case_insensitive_back_reference_in_a_where(dsn: str) -> None:
    """A table's WHERE runs the pattern in `secantus-core`."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists r1022")
        c.execute("create table r1022 (x text)")
        c.execute("insert into r1022 values ('ABab'), ('abab'), ('abAB'), ('abcd')")
        rows = c.execute(r"select x from r1022 where x ~* '(ab)\1' order by x").fetchall()
        count = c.execute(r"select count(*) from r1022 where x !~* '(ab)\1'").fetchone()
    assert (rows, count) == ([("ABab",), ("abAB",), ("abab",)], (1,))


# A regex's flavour and flags (`regcomp.c`'s `prefixes()`): a `***`
# director, an ARE's embedded options, and the newline-insensitive default
# under which `.` matches a newline. Measured on PostgreSQL 15.20.
FLAVOURS = {
    r"select E'a\nb' ~ 'a.b' as m": ([(True,)], ["m"]),
    r"select regexp_replace(E'a\nb', 'a.b', 'X') as r": ([("X",)], ["r"]),
    r"select E'a\nb' ~ '(?n)a.b' as m": ([(False,)], ["m"]),
    r"select E'x\nab' ~ '(?n)^ab' as m": ([(True,)], ["m"]),
    r"select E'x\nab' ~ '(?p)^ab' as m": ([(False,)], ["m"]),
    r"select E'x\nab' ~ '(?w)^ab' as m": ([(True,)], ["m"]),
    r"select 'a.c' ~ '(?q)a.c' as m, 'abc' ~ '(?q)a.c' as n": ([(True, False)], ["m", "n"]),
    r"select '^w' ~ '***=^w' as m": ([(True,)], ["m"]),
    r"select 'w' ~ '***:(?i)W' as m": ([(True,)], ["m"]),
    r"select 'w0+' ~ '(?b)w0+' as m, 'w00' ~ '(?b)w0+' as n": ([(True, False)], ["m", "n"]),
    r"select 'wd' ~ '(?e)w\d' as m": ([(True,)], ["m"]),
    r"select 'W' ~* '(?c)w' as m": ([(False,)], ["m"]),
}


@pytest.mark.backlog("RP-1122")
@pytest.mark.parametrize("query", list(FLAVOURS))
def test_regex_flavours(query: str, dsn: str) -> None:
    """Each was 22P02 "unrecognized flag", or `f` for the newline."""
    assert answer(dsn, query) == FLAVOURS[query]


# `regcomp`'s refusals: 2201B with `pg_regerror`'s text. Measured on
# PostgreSQL 15.20.
INVALID = {
    "select 'a' ~ '(' as m": (
        "2201B",
        "invalid regular expression: parentheses () not balanced",
        None,
    ),
    "select 'a' ~ '(?z)a' as m": (
        "2201B",
        "invalid regular expression: invalid embedded option",
        None,
    ),
    "select 'a' ~ 'a{2,1}' as m": (
        "2201B",
        "invalid regular expression: invalid repetition count(s)",
        None,
    ),
}


@pytest.mark.backlog("RP-1123")
@pytest.mark.parametrize("query", list(INVALID))
def test_invalid_regexes(query: str, dsn: str) -> None:
    """Each was 22P02 with the `regex` crate's message, or `f`."""
    assert answer(dsn, query) == INVALID[query]
