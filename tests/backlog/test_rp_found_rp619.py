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


@pytest.mark.backlog("RP-958")
def test_expression_syntax_error_is_placed_in_the_body(dsn: str) -> None:
    assert _answers(dsn, EXPRESSION_SYNTAX) == [
        ("42601", "syntax error at end of input", "75", None),
        ("42601", "syntax error at end of input", "79", None),
        ("42601", "syntax error at end of input", "27", None),
        ("42601", "syntax error at end of input", "24", None),
    ]


@pytest.mark.backlog("RP-959")
def test_do_block_takes_compile_options(dsn: str) -> None:
    assert _answers(dsn, DO_COMPILE_OPTIONS) == ["ok", "ok"]


@pytest.mark.backlog("RP-960")
def test_return_with_a_value_in_a_void_routine(dsn: str) -> None:
    refused = "RETURN cannot have a parameter in function returning void"
    assert _answers(dsn, RETURN_IN_VOID) == [
        ("42804", refused, "20", None),
        ("42804", refused, "20", None),
        ("42804", refused, "73", None),
    ]
