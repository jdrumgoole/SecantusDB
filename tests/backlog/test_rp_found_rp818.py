"""Regression tests of a divergence found while working RP-818 (a
DECLARE's `%TYPE` was not resolved): a routine's parameter or result
declared `%TYPE` was not resolved either (RP-822, fixed).

CREATE FUNCTION resolves `t.c%TYPE` in a parameter or RETURNS clause to
the column's type, without its modifier, and says so in a NOTICE
(`LookupTypeNameExtended`). The server stored the text `t.v` as the type
instead: a call saw a `record`, and a function returning `t.n%TYPE`
returned NULL. The expected answers were measured on a private, freshly
initdb'd PostgreSQL 15.19 cluster with `_SETUP` and `_WIDE_SETUP`.
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


@pytest.mark.backlog_fixed("RP-822")
@pytest.mark.parametrize("query", list(PARAMETER_PERCENT_TYPE))
def test_a_routines_percent_type_parameter_is_resolved(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == PARAMETER_PERCENT_TYPE[query]


@pytest.mark.backlog_fixed("RP-822")
def test_create_function_reports_the_conversion(dsn: str) -> None:
    notices: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        c.execute(_SETUP[0])
        c.execute(_SETUP[1])
        notices.clear()
        c.execute(_SETUP[2])
    assert notices == ["type reference t821.v%TYPE converted to character varying"]


_WIDE_SETUP = [
    "drop table if exists t822 cascade",
    "drop domain if exists d822 cascade",
    "drop type if exists e822 cascade",
    "drop schema if exists s822 cascade",
    "create schema s822",
    "create domain d822 as varchar(5)",
    "create type e822 as enum ('a', 'b')",
    "create table t822 (a int, v varchar(3), n numeric(5,2), d d822, e e822, "
    "arr int[], c char(4), ts timestamp(2))",
    "create table s822.u822 (x text)",
]

_WIDE_ROUTINES = [
    "create or replace function f822a(x t822.n%type, y t822.d%type, z t822.e%type, "
    "w t822.arr%type, q t822.c%type, r t822.ts%type) returns t822.n%type "
    "language plpgsql as $$ begin return x; end $$",
    "create or replace function f822b(x s822.u822.x%type) returns setof t822.a%type "
    "language sql as $$ select 1 $$",
    "create or replace function f822c(x t822.v%type) returns table (o t822.n%type) "
    "language sql as $$ select 1.5 $$",
    "create or replace function f822d(x t822.ctid%type) returns text "
    "language sql as $$ select pg_typeof(x)::text $$",
    "create or replace function f822e(x pg_catalog.pg_class.relname%type) returns text "
    "language sql as $$ select pg_typeof(x)::text $$",
    "create or replace function f822f(variadic x t822.arr%type) returns int "
    "language sql as $$ select array_length(x, 1) $$",
    "create or replace function f822g(x postgres.s822.u822.x%type) returns text "
    "language sql as $$ select pg_typeof(x)::text $$",
]


@pytest.fixture(scope="module")
def wide_dsn(rust_pg: object) -> Iterator[str]:
    dsn = _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _WIDE_SETUP + _WIDE_ROUTINES:
            c.execute(q)
    yield dsn
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _WIDE_SETUP[:4]:
            c.execute(q)


def _signature(name: str) -> str:
    return (
        f"select pg_get_function_arguments('{name}'::regproc), "
        f"pg_get_function_result('{name}'::regproc)"
    )


WIDE_ANSWERS = {
    _signature("f822a"): [
        (
            "x numeric, y d822, z e822, w integer[], q character, r timestamp without time zone",
            "numeric",
        )
    ],
    "select f822a(1.2345, 'abc', 'a', array[1], 'x', null)::text, "
    "pg_typeof(f822a(1.2345, 'abc', 'a', array[1], 'x', null))::text": [("1.2345", "numeric")],
    _signature("f822b"): [("x text", "SETOF integer")],
    _signature("f822c"): [("x character varying", "TABLE(o numeric)")],
    "select f822d('(0,1)')": [("tid",)],
    "select f822e('a')": [("name",)],
    "select f822f(1, 2)": [(2,)],
    "select f822g('a')": [("text",)],
}


@pytest.mark.backlog_fixed("RP-822")
@pytest.mark.parametrize("query", list(WIDE_ANSWERS))
def test_percent_type_over_every_kind_of_column(query: str, wide_dsn: str) -> None:
    with psycopg.connect(wide_dsn, autocommit=True) as c:
        assert c.execute(query).fetchall() == WIDE_ANSWERS[query]


@pytest.mark.backlog_fixed("RP-822")
def test_every_percent_type_reports_its_conversion_in_order(wide_dsn: str) -> None:
    """The parameters' in order, then the result's; RETURNS TABLE of one
    column looks that column's type up again as the result."""
    notices: list[str] = []
    with psycopg.connect(wide_dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        c.execute(_WIDE_ROUTINES[0])
        c.execute(_WIDE_ROUTINES[2])
    assert notices == [
        "type reference t822.n%TYPE converted to numeric",
        "type reference t822.d%TYPE converted to d822",
        "type reference t822.e%TYPE converted to e822",
        "type reference t822.arr%TYPE converted to integer[]",
        "type reference t822.c%TYPE converted to character",
        "type reference t822.ts%TYPE converted to timestamp without time zone",
        "type reference t822.n%TYPE converted to numeric",
        "type reference t822.v%TYPE converted to character varying",
        "type reference t822.n%TYPE converted to numeric",
        "type reference t822.n%TYPE converted to numeric",
    ]


_REFUSED_BODY = " returns int language sql as $$ select 1 $$"

REFUSED = {
    "create function r822(x other.s822.u822.x%type)": (
        "0A000",
        'cross-database references are not implemented: "other.s822.u822"',
    ),
    "create function r822(x a.b.c.d.e%type)": (
        "42601",
        "improper %TYPE reference (too many dotted names): a.b.c.d.e",
    ),
    "create function r822(x t822.nope%type)": (
        "42703",
        'column "nope" of relation "t822" does not exist',
    ),
    "create function r822(x s822.u822.nope%type)": (
        "42703",
        'column "nope" of relation "u822" does not exist',
    ),
    "create function r822(x nope822.v%type)": ("42P01", 'relation "nope822" does not exist'),
    'create function r822(x "T822".v%type)': ("42P01", 'relation "T822" does not exist'),
    "create function r822(x nos822.nope.v%type)": ("3F000", 'schema "nos822" does not exist'),
    "create function r822(x pg_temp.t822.v%type)": ("3F000", 'schema "pg_temp" does not exist'),
}


@pytest.mark.backlog_fixed("RP-822")
@pytest.mark.parametrize("head", list(REFUSED))
def test_a_percent_type_nothing_answers_is_refused_unpositioned(head: str, wide_dsn: str) -> None:
    with psycopg.connect(wide_dsn, autocommit=True) as c, pytest.raises(psycopg.Error) as e:
        c.execute(head + _REFUSED_BODY)
    got = (e.value.sqlstate, e.value.diag.message_primary, e.value.diag.statement_position)
    assert got == (*REFUSED[head], None)


@pytest.mark.backlog_fixed("RP-822")
def test_a_conversion_is_reported_before_a_later_parameters_error(wide_dsn: str) -> None:
    notices: list[str] = []
    with psycopg.connect(wide_dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        with pytest.raises(psycopg.Error) as e:
            c.execute("create function r822(x t822.v%type, y nope_type822)" + _REFUSED_BODY)
    assert (e.value.sqlstate, e.value.diag.message_primary) == (
        "42704",
        "type nope_type822 does not exist",
    )
    assert notices == ["type reference t822.v%TYPE converted to character varying"]


@pytest.mark.backlog_fixed("RP-822")
def test_a_temporary_tables_column_is_found_through_pg_temp(wide_dsn: str) -> None:
    with psycopg.connect(wide_dsn, autocommit=True) as c:
        c.execute("create temp table tt822 (v numeric(4,1))")
        c.execute(
            "create or replace function f822t(x pg_temp.tt822.v%type) returns text "
            "language sql as $$ select pg_typeof(x)::text $$"
        )
        try:
            assert c.execute("select f822t(1)").fetchall() == [("numeric",)]
        finally:
            c.execute("drop function f822t")


@pytest.mark.backlog_fixed("RP-822")
def test_a_declare_finds_a_catalog_column_qualified_by_pg_catalog(wide_dsn: str) -> None:
    """The DECLARE's `%TYPE` (RP-818) shares the relation lookup, which
    keyed `pg_catalog.pg_class` under the schema and found no catalog."""
    notices: list[str] = []
    with psycopg.connect(wide_dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        c.execute(
            "do $$ declare x pg_catalog.pg_class.relname%type; "
            "begin raise notice '%', pg_typeof(x); end $$"
        )
    assert notices == ["name"]
