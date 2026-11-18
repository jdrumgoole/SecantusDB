"""Gap tests of what was found while working RP-619 (a DECLARE's missing
type was reported after a later syntax error).

Each answer is `"ok"`, or `(sqlstate, message, position, context)` for an
error. The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster, each in a new session.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _answers(dsn: str, queries: list[str]) -> list[Any]:
    """Each statement's answer, in a session of its own, the routine dropped."""
    out: list[Any] = []
    for query in queries:
        with psycopg.connect(dsn, autocommit=True) as c:
            try:
                c.execute(query)
                out.append("ok")
            except psycopg.Error as e:
                d = e.diag
                out.append((e.sqlstate, d.message_primary, d.statement_position, d.context))
            c.execute("drop function if exists e619")
    return out


# `check_sql_expr` parses each expression as the compiler reads it, and
# `plpgsql_scanner_errposition` places its syntax error inside the body.
EXPRESSION_SYNTAX = [
    "create function e619() returns int language plpgsql as $$ begin return 1 +; end $$",
    "create function e619() returns int language plpgsql as $$ declare x int := 1 +; "
    "begin return 1; end $$",
    "do $$ declare x int := 1 +; begin null; end $$",
    "do $$ begin perform 1 +; end $$",
]

# `plpgsql_compile_inline` reads the same grammar as a function's body,
# its `#option` lines included.
DO_COMPILE_OPTIONS = [
    "do $$ #variable_conflict use_column\n begin null; end $$",
    "do $$ #print_strict_params on\n declare x int; begin null; end $$",
]

# `make_return_stmt` refuses an expression in a routine returning void as
# it reads the RETURN, before the expression is parsed.
RETURN_IN_VOID = [
    "do $$ begin return 1; end $$",
    "do $$ begin return 1 +; end $$",
    "create function e619() returns void language plpgsql as $$ begin return 1 +; end $$",
]


# Each shape `make_return_stmt` sorts a routine into, and a syntax error
# before the RETURN reported first.
RETURN_SHAPES = [
    (
        "create procedure p960() language plpgsql as $$ begin return 1; end $$",
        ("42601", "RETURN cannot have a parameter in a procedure", "61", None),
    ),
    (
        "create function f960a() returns setof int language plpgsql as $$ begin return 1; end $$",
        (
            "42804",
            "RETURN cannot have a parameter in function returning set",
            "79",
            "Use RETURN NEXT or RETURN QUERY.",
        ),
    ),
    (
        "create function f960b(out a int) language plpgsql as $$ begin return 1; end $$",
        ("42804", "RETURN cannot have a parameter in function with OUT parameters", "70", None),
    ),
    (
        "create function f960c() returns void language plpgsql as $$ begin if true then "
        "return 1 +; end if; end $$",
        ("42804", "RETURN cannot have a parameter in function returning void", "87", None),
    ),
    (
        "create function f960d() returns void language plpgsql as $$ begin selec 1; "
        "return 1; end $$",
        ("42601", 'syntax error at or near "selec"', "67", None),
    ),
    (
        "create function f960e() returns void language plpgsql as $$ begin return; end $$",
        "ok",
    ),
    (
        "create function f960f() returns int language plpgsql as $$ begin return 1; end $$",
        "ok",
    ),
]


@pytest.mark.backlog_fixed("RP-958")
def test_expression_syntax_error_is_placed_in_the_body(dsn: str) -> None:
    """A DO block's was at the statement's end, or found when it ran; now
    in the body, at compile."""
    assert _answers(dsn, EXPRESSION_SYNTAX) == [
        ("42601", "syntax error at end of input", "75", None),
        ("42601", "syntax error at end of input", "79", None),
        ("42601", "syntax error at end of input", "27", None),
        ("42601", "syntax error at end of input", "24", None),
    ]


@pytest.mark.backlog_fixed("RP-959")
def test_do_block_takes_compile_options(dsn: str) -> None:
    """Was 42601 at the `#`; the block now runs on the function grammar."""
    assert _answers(dsn, DO_COMPILE_OPTIONS) == ["ok", "ok"]


@pytest.mark.backlog_fixed("RP-960")
def test_return_with_a_value_in_a_void_routine(dsn: str) -> None:
    """Was accepted, or a later syntax error; now refused at the RETURN."""
    refused = "RETURN cannot have a parameter in function returning void"
    assert _answers(dsn, RETURN_IN_VOID) == [
        ("42804", refused, "20", None),
        ("42804", refused, "20", None),
        ("42804", refused, "73", None),
    ]


@pytest.mark.backlog_fixed("RP-960")
@pytest.mark.parametrize(("query", "expected"), RETURN_SHAPES)
def test_return_expression_by_routine_shape(dsn: str, query: str, expected: Any) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            c.execute(query)
            got: Any = "ok"
        except psycopg.Error as e:
            d = e.diag
            got = (e.sqlstate, d.message_primary, d.statement_position, d.message_hint)
    assert got == expected


# A grammar error without a scanner position is framed by the compile,
# `near` the line the compiler had reached, and not placed.
UNKNOWN_CONDITION = 'unrecognized exception condition "unknown_thing"'


@pytest.mark.backlog_fixed("RP-958")
def test_do_block_compile_error_has_context(dsn: str) -> None:
    """A DO block's compile error, checked before the block runs, keeps
    the compile context (it had none on the function interpreter path)."""
    assert _answers(
        dsn,
        [
            "do $$ declare x int; begin raise unknown_thing; end $$",
            "do $$ begin\n null;\n raise unknown_thing; end $$",
        ],
    ) == [
        (
            "42704",
            UNKNOWN_CONDITION,
            None,
            f'compilation of PL/pgSQL function "inline_code_block" near line {n}',
        )
        for n in (1, 3)
    ]


@pytest.mark.backlog_fixed("RP-1040")
def test_create_function_compile_error_has_context(dsn: str) -> None:
    assert _answers(
        dsn,
        [
            "create function e619() returns void language plpgsql as $$ begin "
            "raise unknown_thing; end $$"
        ],
    ) == [
        ("42704", UNKNOWN_CONDITION, None, 'compilation of PL/pgSQL function "e619" near line 1'),
    ]
