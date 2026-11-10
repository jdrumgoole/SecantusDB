"""Gap test of a divergence found while working RP-818 (a DECLARE's
`%TYPE` was not resolved): a routine's parameter or result declared
`%TYPE` is not resolved either.

CREATE FUNCTION resolves `t.c%TYPE` in a parameter or RETURNS clause to
the column's type, without its modifier, and says so in a NOTICE
(`LookupTypeNameExtended`). The expected answers were measured on a
private, freshly initdb'd PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t821 cascade",
    "create table t821 (a int, v varchar(3))",
    "create or replace function f821(x t821.v%type) returns text language plpgsql "
    "as $$ begin return pg_typeof(x)::text; end $$",
    "create or replace function g821(x int) returns t821.v%type language plpgsql "
    "as $$ begin return 'abcd'; end $$",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])
        c.execute("drop function if exists f821")
        c.execute("drop function if exists g821")


def _answer(dsn: str, query: str) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            try:
                c.execute(q)
            except psycopg.Error as e:
                return ("setup error", e.sqlstate, e.diag.message_primary)
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return ("error", e.sqlstate, e.diag.message_primary)


PARAMETER_PERCENT_TYPE = {
    "select f821('ab')": [("character varying",)],
    "select pg_get_function_arguments('f821'::regproc)": [("x character varying",)],
    "select g821(1)::text, pg_typeof(g821(1))::text": [("abcd", "character varying")],
}


@pytest.mark.backlog("RP-822")
@pytest.mark.parametrize("query", list(PARAMETER_PERCENT_TYPE))
def test_a_routines_percent_type_parameter_is_resolved(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == PARAMETER_PERCENT_TYPE[query]


@pytest.mark.backlog("RP-822")
def test_create_function_reports_the_conversion(dsn: str) -> None:
    notices: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        c.execute(_SETUP[0])
        c.execute(_SETUP[1])
        notices.clear()
        c.execute(_SETUP[2])
    assert notices == ["type reference t821.v%TYPE converted to character varying"]
