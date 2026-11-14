"""Gap tests of the divergences found while working RP-515 (a routine's
argument list printed without modes): RP-876 (`pg_get_function_arguments`
and `pg_get_function_result` print names unquoted) and RP-877 (a
procedure's `pg_get_function_result` and `pg_get_functiondef` read as a
function's).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster after `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_ROUTINES = ['"F 4"(int, int)', "gt(int)", "gk(int, text)"]

_SETUP = [f"drop routine if exists {r}" for r in _ROUTINES] + [
    'create function "F 4"("A b" int, "select" int) returns int language sql as $$ select 1 $$',
    'create function gt(a int) returns table(x int, "Y z" text) '
    "language sql as $$ select 1, 'x'::text $$",
    "create procedure gk(a int, inout b text) language sql as $$ select 'x' $$",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    conninfo = _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(conninfo, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
    yield conninfo
    with psycopg.connect(conninfo, autocommit=True) as c:
        for r in _ROUTINES:
            c.execute(f"drop routine if exists {r}")


def rows(dsn: str, query: str) -> list[tuple[Any, ...]]:
    with psycopg.connect(dsn, autocommit=True) as c:
        return c.execute(query).fetchall()


# RP-876: `print_function_arguments` and `print_function_rettype` quote
# each parameter's and TABLE column's name with `quote_identifier`.
@pytest.mark.backlog("RP-876")
def test_argument_and_column_names_are_quoted(dsn: str) -> None:
    assert rows(
        dsn,
        "select pg_get_function_arguments('\"F 4\"'::regproc), "
        "pg_get_function_identity_arguments('\"F 4\"'::regproc), "
        "pg_get_function_result('gt'::regproc)",
    ) == [
        (
            '"A b" integer, "select" integer',
            '"A b" integer, "select" integer',
            'TABLE(x integer, "Y z" text)',
        )
    ]


# RP-877 (fixed): a procedure had a result type (`record`) and
# `pg_get_functiondef` printed it as a FUNCTION; PostgreSQL's
# `pg_get_function_result` is NULL and its definition a PROCEDURE's.
@pytest.mark.backlog_fixed("RP-877")
def test_procedure_result_and_definition(dsn: str) -> None:
    assert rows(
        dsn,
        "select pg_get_function_result('gk'::regproc), pg_get_functiondef('gk'::regproc)",
    ) == [
        (
            None,
            "CREATE OR REPLACE PROCEDURE public.gk(IN a integer, INOUT b text)\n"
            " LANGUAGE sql\n"
            "AS $procedure$ select 'x' $procedure$\n",
        )
    ]
