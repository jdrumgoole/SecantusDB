"""Gap tests of the function-body divergences found while working RP-451:
RP-461 (a SQL function's bare parameter name is replaced wherever it is a
word), RP-462 (a composite parameter's field was not read; fixed), RP-463 (a
PL/pgSQL block label does not qualify its variables), RP-464 (a SQL
function ending in a write's RETURNING returned NULL; fixed) and RP-465 (CREATE
FUNCTION does not check a SQL body).

An answer is the rows, `("rowcount", n)` for a statement with none, or
`(sqlstate, message, hint, position)` for an error. The expected answers
were measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with
`_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_FUNCTIONS = ("gb(int)", "gal(int)", "gr1(fe)", "gr2(fe)", "gl(int)", "gi(int)")
_FUNCTIONS += ("gi2(int)", "gu(int)", "gd(int)", "gx(int)")

_SETUP = [
    *(f"drop function if exists {f}" for f in _FUNCTIONS),
    "drop table if exists fe, fs cascade",
    "create table fe (a int, b text)",
    "create table fs (a int, c text)",
    "insert into fe values (1, 'x')",
    "insert into fs values (1, 'y')",
    "create function gb(a int) returns int language sql as $$ select a from fe $$",
    "create function gal(p int) returns int language sql as $$ select 1 as p $$",
    "create function gr1(r fe) returns int language sql as $$ select r.a $$",
    "create function gr2(r fe) returns int language sql as $$ select gr2.r.a $$",
    "create function gl(p int) returns int language plpgsql"
    " as $$ <<blk>> declare x int := 3; begin return blk.x + p; end $$",
    "create function gi(p int) returns int language sql"
    " as $$ insert into fs values (p, 'i') returning p * 2 $$",
    "create function gi2(p int) returns int language sql"
    " as $$ insert into fs values (p, 'i') returning a * 2 $$",
    "create function gu(p int) returns int language sql"
    " as $$ update fs set a = p returning a * 2 $$",
    "create function gd(p int) returns text language sql"
    " as $$ delete from fs where a = p returning c $$",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for f in _FUNCTIONS:
            c.execute(f"drop function if exists {f}")
        c.execute("drop table if exists fe, fs cascade")


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )
        return cur.fetchall() if cur.description else ("rowcount", cur.rowcount)


# A column of the name comes first, and an output alias is no reference.
BARE_PARAMETER_NAME = {
    "select gb(7)": [(1,)],
    "select gal(7)": [(1,)],
}


@pytest.mark.backlog("RP-461")
@pytest.mark.parametrize("query", list(BARE_PARAMETER_NAME))
def test_bare_parameter_name(query: str, dsn: str) -> None:
    assert answer(dsn, query) == BARE_PARAMETER_NAME[query]


COMPOSITE_PARAMETER_FIELD = {
    "select gr1(row(2, 'z')::fe)": [(2,)],
    "select gr2(row(2, 'z')::fe)": [(2,)],
}


@pytest.mark.backlog_fixed("RP-462")
@pytest.mark.parametrize("query", list(COMPOSITE_PARAMETER_FIELD))
def test_composite_parameter_field(query: str, dsn: str) -> None:
    """`r.a` and `gr2.r.a` over a composite parameter `r` were 42P01
    `missing FROM-clause entry for table "r"`: the body bound only a
    parameter's bare name and `fname.p`, never a field of one."""
    assert answer(dsn, query) == COMPOSITE_PARAMETER_FIELD[query]


@pytest.mark.backlog("RP-463")
def test_block_label_qualifies_a_variable(dsn: str) -> None:
    assert answer(dsn, "select gl(7)") == [(10,)]


RETURNING_RESULT = {
    "select gi(4)": [(8,)],
    "select gi2(4)": [(8,)],
    "select gu(4)": [(8,)],
    "select gd(1)": [("y",)],
}


@pytest.mark.backlog_fixed("RP-464")
@pytest.mark.parametrize("query", list(RETURNING_RESULT))
def test_returning_statement_is_the_result(query: str, dsn: str) -> None:
    """A body ending in a write's RETURNING answered NULL: the result path
    ran the write for its effect and dropped its rows."""
    assert answer(dsn, query) == RETURNING_RESULT[query]


# More shapes of a write's RETURNING as the body's result, over a table of
# three rows, measured on the same private PostgreSQL 15.19 cluster: a
# scalar function's write runs to completion though only its first row is
# the value; a RETURNING before the last statement is not the result; a
# write that returns no row is NULL; a RETURNING list may be an expression.
_RETURNING_SETUP = [
    "drop function if exists gm(int), gmid(int), gn(int), gw(int)",
    "drop table if exists t464",
    "create table t464 (a int, b text)",
    "insert into t464 values (1, 'p'), (2, 'q'), (3, 'r')",
    "create function gm(p int) returns int language sql"
    " as $$ update t464 set a = a + p returning a $$",
    "create function gmid(p int) returns int language sql"
    " as $$ insert into t464 values (p, 'm') returning a; select count(*)::int from t464 $$",
    "create function gn(p int) returns int language sql"
    " as $$ delete from t464 where a = p returning a $$",
    "create function gw(p int) returns text language sql"
    " as $$ insert into t464 values (p, 'w') returning b || a $$",
]

RETURNING_SHAPES = {
    "select gm(10), (select array_agg(a order by a) from t464)": [(11, [1, 2, 3])],
    "select gm(10); select array_agg(a order by a) from t464": [([11, 12, 13],)],
    "select gmid(9)": [(4,)],
    "select gn(99)": [(None,)],
    "select gw(5), (select count(*) from t464)": [("w5", 3)],
}


@pytest.mark.backlog_fixed("RP-464")
@pytest.mark.parametrize("query", list(RETURNING_SHAPES))
def test_returning_shapes(query: str, dsn: str) -> None:
    """Each shape answered NULL, or the count without the middle write."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _RETURNING_SETUP:
            c.execute(q)
        try:
            *statements, last = query.split("; ")
            for q in statements:
                c.execute(q)
            got = c.execute(last).fetchall()
        finally:
            c.execute("drop function if exists gm(int), gmid(int), gn(int), gw(int)")
            c.execute("drop table if exists t464")
    assert got == RETURNING_SHAPES[query]


_CREATE_GX = "create function gx(p int) returns int language sql as $$ {} $$"

CREATE_CHECKS_THE_BODY = {
    _CREATE_GX.format("select gx.q"): (
        "42P01",
        'missing FROM-clause entry for table "gx"',
        None,
        "65",
    ),
    _CREATE_GX.format("select nosuch"): ("42703", 'column "nosuch" does not exist', None, "65"),
    _CREATE_GX.format("select 1 from nosuch"): (
        "42P01",
        'relation "nosuch" does not exist',
        None,
        "72",
    ),
}


@pytest.mark.backlog("RP-465")
@pytest.mark.parametrize("query", list(CREATE_CHECKS_THE_BODY))
def test_create_checks_the_body(query: str, dsn: str) -> None:
    assert answer(dsn, query) == CREATE_CHECKS_THE_BODY[query]
