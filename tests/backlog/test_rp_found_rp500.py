"""Gap tests of the divergences found while working RP-500 (CREATE FUNCTION
checks a SQL body's constants, CALLs and result type): RP-507 (a constant
an operator over a column or a parameter, an INSERT or UPDATE target or a
COALESCE gives its type is not read at CREATE), RP-508 (DROP FUNCTION /
PROCEDURE without arguments over the other kind, and DROP ROUTINE's
notice), RP-509 (a CALL that leaves a defaulted parameter out, names its
arguments, or passes a column), RP-510 (a malformed range literal had no DETAIL, fixed)
and RP-511 (PREPARE evaluates a constant expression and does not read a
cast constant it does not evaluate).

An answer is `("ok", rows, notices)`, or `(sqlstate, message, detail, hint,
position, notices)` for an error. The expected answers were measured on a
private, freshly initdb'd PostgreSQL 15.19 cluster after `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop routine if exists gq(int)",
    "drop routine if exists gq2(int)",
    "drop routine if exists gx(int)",
    "drop procedure if exists pd(int, int)",
    "drop procedure if exists gvp(int)",
    "drop routine if exists gm(int)",
    "drop routine if exists gm(text)",
    "drop routine if exists gz(int)",
    "drop routine if exists gp(int)",
    "drop routine if exists gp(text)",
    "drop table if exists fe cascade",
    "create table fe (a int, b text)",
    "create procedure gq(int) language sql as $$ select 1 $$",
    "create function gq2(int) returns int language sql as $$ select 1 $$",
    "create procedure pd(a int, b int default 2) language sql as $$ select 1 $$",
    "create procedure gvp(x int) language sql as $$ select 1 $$",
    # A function and a procedure of one name, and two procedures of one.
    "create function gm(int) returns int language sql as $$ select 1 $$",
    "create procedure gm(text) language sql as $$ select 1 $$",
    "create procedure gp(int) language sql as $$ select 1 $$",
    "create procedure gp(text) language sql as $$ select 1 $$",
]

_TEARDOWN = [
    "drop routine if exists gq(int)",
    "drop routine if exists gq2(int)",
    "drop routine if exists gx(int)",
    "drop procedure if exists pd(int, int)",
    "drop procedure if exists gvp(int)",
    "drop routine if exists gm(int)",
    "drop routine if exists gm(text)",
    "drop routine if exists gz(int)",
    "drop routine if exists gp(int)",
    "drop routine if exists gp(text)",
    "drop table if exists fe cascade",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _TEARDOWN:
            c.execute(q)


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True, prepare_threshold=None) as c:
        for q in _SETUP:
            c.execute(q)
        notices: list[str | None] = []
        c.add_notice_handler(lambda d: notices.append(d.message_primary))
        try:
            cur = c.execute(query)
            return ("ok", cur.fetchall() if cur.description else None, notices)
        except psycopg.Error as e:
            d = e.diag
            return (
                e.sqlstate,
                d.message_primary,
                d.message_detail,
                d.message_hint,
                d.statement_position,
                notices,
            )


def _input(value: str, position: str) -> tuple[Any, ...]:
    return ("22P02", f'invalid input syntax for type integer: "{value}"', None, None, position, [])


_GX = "create function gx(p int) returns {} language sql as $$ {} $$"

# RP-507: `transformExpr` coerces each of these constants to the type the
# operator, the target column or COALESCE's other argument gives it, and
# its input function refuses the text, at CREATE.
OPERAND_CONSTANT_READ_AT_CREATE = {
    _GX.format("int", "select p + 'a'"): _input("a", "69"),
    _GX.format("int", "select a + 'q' from fe"): _input("q", "69"),
    _GX.format("void", "insert into fe values ('q')"): _input("q", "82"),
    _GX.format("void", "update fe set a = 'q'"): _input("q", "77"),
    _GX.format("int", "select coalesce('q', 1)"): _input("q", "74"),
}


@pytest.mark.backlog("RP-507")
@pytest.mark.parametrize("query", list(OPERAND_CONSTANT_READ_AT_CREATE))
def test_operand_constant_read_at_create(query: str, dsn: str) -> None:
    assert answer(dsn, query) == OPERAND_CONSTANT_READ_AT_CREATE[query]


# RP-508 (fixed): a routine named without arguments was looked up among
# every routine, so the other kind was the wrong kind (42809) where
# PostgreSQL's `LookupFuncWithArgs` looks among the statement's kind only
# and finds it missing; and DROP ROUTINE's notice said "function".
DROP_WITHOUT_ARGUMENTS = {
    "drop function if exists gq": ("ok", None, ["function gq() does not exist, skipping"]),
    "drop function gq": ("42883", 'could not find a function named "gq"', None, None, None, []),
    "drop procedure if exists gq2": ("ok", None, ["procedure gq2() does not exist, skipping"]),
    "drop procedure gq2": (
        "42883",
        'could not find a procedure named "gq2"',
        None,
        None,
        None,
        [],
    ),
    "drop routine if exists nosuchr(int)": (
        "ok",
        None,
        ["routine nosuchr(pg_catalog.int4) does not exist, skipping"],
    ),
}


@pytest.mark.backlog_fixed("RP-508")
@pytest.mark.parametrize("query", list(DROP_WITHOUT_ARGUMENTS))
def test_drop_without_arguments_over_the_other_kind(query: str, dsn: str) -> None:
    assert answer(dsn, query) == DROP_WITHOUT_ARGUMENTS[query]


def _not_unique(kind: str, name: str) -> tuple[Any, ...]:
    return (
        "42725",
        f'{kind} name "{name}" is not unique',
        None,
        f"Specify the argument list to select the {kind} unambiguously.",
        None,
        [],
    )


# RP-508 (fixed), its neighbours: the same kind filter picks the one routine
# of the statement's kind out of a function and a procedure of one name
# (`drop function gm` drops `gm(int)` and keeps the procedure), for ALTER as
# for DROP; and the 42725 for two of the kind names the statement's kind.
LOOKUP_BY_KIND = {
    "drop function gm": ("ok", None, []),
    "drop procedure gm": ("ok", None, []),
    "drop routine gm": _not_unique("routine", "gm"),
    "drop procedure gp": _not_unique("procedure", "gp"),
    "drop routine if exists nosuchr": ("ok", None, ["routine nosuchr() does not exist, skipping"]),
    "drop function gp": ("42883", 'could not find a function named "gp"', None, None, None, []),
    "alter function gq rename to gz": (
        "42883",
        'could not find a function named "gq"',
        None,
        None,
        None,
        [],
    ),
    "alter procedure gm rename to gz": ("ok", None, []),
    "alter routine gm rename to gz": _not_unique("routine", "gm"),
    "alter procedure gp rename to gz": _not_unique("procedure", "gp"),
}


@pytest.mark.backlog_fixed("RP-508")
@pytest.mark.parametrize("query", list(LOOKUP_BY_KIND))
def test_routine_lookup_by_kind(query: str, dsn: str) -> None:
    assert answer(dsn, query) == LOOKUP_BY_KIND[query]


@pytest.mark.backlog_fixed("RP-508")
def test_drop_function_keeps_the_procedure_of_its_name(dsn: str) -> None:
    """`drop function gm` over a function `gm(int)` and a procedure
    `gm(text)` drops the function only (PostgreSQL 15.19)."""
    assert answer(dsn, "drop function gm") == ("ok", None, [])
    with psycopg.connect(dsn, autocommit=True) as c:
        rows = c.execute(
            "select prokind, oid::regprocedure::text from pg_proc where proname = 'gm'"
        ).fetchall()
    assert rows == [("p", "gm(text)")]


# RP-509 (fixed): a CALL's defaulted parameter, its named arguments, and a
# column reference among its arguments were refused; they are resolved as
# `FuncnameGetCandidates` and `transformCallStmt` resolve them.
CALL_ARGUMENTS = {
    "call pd(1)": ("ok", None, []),
    "call pd(b => 3, a => 1)": ("ok", None, []),
    "call gvp(nosuch)": ("42703", 'column "nosuch" does not exist', None, None, "10", []),
}


@pytest.mark.backlog_fixed("RP-509")
@pytest.mark.parametrize("query", list(CALL_ARGUMENTS))
def test_call_arguments(query: str, dsn: str) -> None:
    assert answer(dsn, query) == CALL_ARGUMENTS[query]


# RP-510: a malformed range literal had no DETAIL; it carries
# `range_parse`'s now.
RANGE_LITERAL_DETAIL = {
    "select '[1,2'::int4range": ("[1,2", "Unexpected end of input."),
    "select '[1,2]x'::int4range": ("[1,2]x", "Junk after right parenthesis or bracket."),
    "select '1,2'::int4range": ("1,2", "Missing left parenthesis or bracket."),
}


@pytest.mark.backlog_fixed("RP-510")
@pytest.mark.parametrize("query", list(RANGE_LITERAL_DETAIL))
def test_range_literal_detail(query: str, dsn: str) -> None:
    value, detail = RANGE_LITERAL_DETAIL[query]
    assert answer(dsn, query) == (
        "22P02",
        f'malformed range literal: "{value}"',
        detail,
        None,
        "8",
        [],
    )


# RP-511: PREPARE parse-analyses its statement and plans nothing, so it
# evaluates no expression (`1/0` is prepared) but reads every constant a
# cast or an operator coerces, evaluated or not.
PREPARE_ANALYSES = {
    "prepare s1 as select 1/0": ("ok", None, []),
    "prepare s2 as select 'x'::int from fe": _input("x", "22"),
    "prepare s3 as select case when false then 'x'::int end": _input("x", "43"),
    "prepare s4(int) as select $1 + 'a'": _input("a", "32"),
}


@pytest.mark.backlog("RP-511")
@pytest.mark.parametrize("query", list(PREPARE_ANALYSES))
def test_prepare_analyses_without_evaluating(query: str, dsn: str) -> None:
    assert answer(dsn, query) == PREPARE_ANALYSES[query]
