"""Gap tests of the function-body divergences found while working RP-451:
RP-461 (a SQL function's bare parameter name was replaced wherever it was
a word; fixed), RP-462 (a composite parameter's field was not read; fixed), RP-463 (a
PL/pgSQL block label did not qualify its variables; fixed), RP-464 (a SQL
function ending in a write's RETURNING returned NULL; fixed) and RP-465 (CREATE
FUNCTION did not check a SQL body; fixed); and found while working RP-461,
RP-477 (a function with one OUT parameter, called in FROM, named its
column after the function; fixed), found while working RP-477, RP-483
(the same function's call in a select list was named after its parameter,
and a field of its scalar result was not refused; fixed), and found while
working RP-483, RP-485 (a call in a schema that does not exist is a missing
function, not a missing schema); and found while working RP-465, RP-500
(CREATE did not check a literal's input, the result type, a CALL, or a
SQL-standard body with `check_function_bodies` off; fixed) and RP-502 (an
array parameter could not be subscripted; fixed).

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
_FUNCTIONS += tuple(f"gn{i}(int)" for i in range(1, 16))
_FUNCTIONS += ("gto()", "gtt()", "gtun()", "gtio(int)", "gtc()", "gts()")

# RP-461's further shapes: which bare names are a column and which the
# parameter, by query level, FROM item and clause.
_BARE_NAME_BODIES = [
    ("gn1(b int) returns int", "select b from fs"),
    ("gn2(n int) returns setof int", "select i * n from generate_series(1, n) i"),
    ("gn3(c int) returns text", "select (select c from fe limit 1) from fs"),
    ("gn4(b int) returns int", "select x from fe, (select b as x) s"),
    ("gn5(key int) returns text", "select key from jsonb_each('{\"k\": 1}')"),
    ("gn6(p int) returns int", "select a as p from fe order by p"),
    ("gn7(a int) returns int", "insert into fs values (a, 'i') returning a * 2"),
    ("gn8(a int) returns int", "update fs set a = a + 10 returning a"),
    ("gn9(a int) returns int", "with w(z) as (select a) select z from w"),
    ("gn10(b int) returns int", "select count(*)::int from fe join fs using (a) where b = 'x'"),
    (
        "gn11(c int) returns int",
        "select a from fs where exists (select 1 from fe where fe.b = 'x' and c = 'y')",
    ),
    ("gn12(c int) returns int", "select a from fe union select a from fs order by a limit c"),
    ("gn13(b int) returns int", "select b from gnv"),
    ("gn14(a int) returns int", "select a + 1 from gnv"),
    ("gn15(b int) returns int", "select b from (select * from fs) s"),
]

_SETUP = [
    *(f"drop function if exists {f}" for f in _FUNCTIONS),
    "drop table if exists fe, fs cascade",
    "drop type if exists gtk",
    "create table fe (a int, b text)",
    "create table fs (a int, c text)",
    "insert into fe values (1, 'x')",
    "insert into fs values (1, 'y')",
    "create view gnv as select a from fe",
    "create function gto(out t int) language sql as $$ select 6 $$",
    "create function gtt() returns table (t int) language sql as $$ select 5 $$",
    "create function gtun(out int) language sql as $$ select 3 $$",
    "create function gtio(inout x int) language sql as $$ select x + 1 $$",
    "create type gtk as (k int)",
    "create function gtc() returns gtk language sql as $$ select row(8)::gtk $$",
    "create function gts() returns int language sql as $$ select 4 $$",
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
    *(f"create function {sig} language sql as $$ {body} $$" for sig, body in _BARE_NAME_BODIES),
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for f in _FUNCTIONS:
            c.execute(f"drop function if exists {f}")
        c.execute("drop table if exists fe, fs cascade")
        c.execute("drop type if exists gtk")


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
    # No column `b` in scope: the parameter.
    "select gn1(7)": [(7,)],
    # A scalar function's column is named by its alias.
    "select gn2(3)": [(3,), (6,), (9,)],
    # The enclosing level's column.
    "select gn3(7)": [("y",)],
    # A FROM-subquery that is not LATERAL does not see its siblings.
    "select gn4(7)": [(7,)],
    # A built-in's OUT parameter is a column.
    "select gn5(7)": [("k",)],
    # ORDER BY an output column's name.
    "select gn6(7)": [(1,)],
    # An INSERT's VALUES does not see its target; its RETURNING does.
    "select gn7(4)": [(8,)],
    # An UPDATE's SET sees its target.
    "select gn8(4)": [(11,)],
    # A CTE's query without FROM; the CTE's own column.
    "select gn9(5)": [(5,)],
    # Both sides of a join.
    "select gn10(7)": [(1,)],
    # A subquery's names, at each level.
    "select gn11(7)": [(1,)],
    # A set operation's LIMIT.
    "select gn12(7)": [(1,)],
    # A view's columns.
    "select gn13(7)": [(7,)],
    "select gn14(7)": [(2,)],
    # A FROM-subquery's `*`: its items' columns.
    "select gn15(7)": [(7,)],
}


@pytest.mark.backlog_fixed("RP-461")
@pytest.mark.parametrize("query", list(BARE_PARAMETER_NAME))
def test_bare_parameter_name(query: str, dsn: str) -> None:
    """Every identifier token naming a parameter became `$n`, so a column
    of the same name read the parameter (`gb(7)` answered 7) and an output
    alias was a syntax error (`gal(7)`)."""
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


@pytest.mark.backlog_fixed("RP-463")
def test_block_label_qualifies_a_variable(dsn: str) -> None:
    """Was 42P01 `missing FROM-clause entry for table "blk"`: a label the
    body wrote qualified none of its block's variables (more shapes:
    `test_rp_plpgsql_labels.py`)."""
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


@pytest.mark.backlog_fixed("RP-465")
@pytest.mark.parametrize("query", list(CREATE_CHECKS_THE_BODY))
def test_create_checks_the_body(query: str, dsn: str) -> None:
    """Each CREATE succeeded, and the error waited for the first call."""
    assert answer(dsn, query) == CREATE_CHECKS_THE_BODY[query]


def _missing_column(position: str) -> tuple[str, str, None, str]:
    return ("42703", 'column "nosuch" does not exist', None, position)


# RP-465's further shapes, each measured on the same cluster: the position
# counted into the CREATE's text through a quoted body's doubled quotes, a
# later statement and a parameter bound by name; a body calling itself, a
# polymorphic one (analysed only when called) and a utility statement's
# names (resolved when it runs); `check_function_bodies` off.
_CREATE_GX_QUOTED = "create function gx(p int) returns int language sql as '{}'"
CREATE_CHECKS_MORE = {
    _CREATE_GX_QUOTED.format("select ''x'' || nosuch"): _missing_column("72"),
    _CREATE_GX.format("select 1; select nosuch"): _missing_column("75"),
    "create function gx(p int) returns int language sql as $$select 1;\n"
    "  select a from fe where b = p and zz = 1 $$": (
        "42883",
        "operator does not exist: text = integer",
        "No operator matches the given name and argument types."
        " You might need to add explicit type casts.",
        "94",
    ),
    _CREATE_GX.format("select nosuchfn(p)"): (
        "42883",
        "function nosuchfn(integer) does not exist",
        "No function matches the given name and argument types."
        " You might need to add explicit type casts.",
        "65",
    ),
    _CREATE_GX.format("select a from fe, fs"): (
        "42702",
        'column reference "a" is ambiguous',
        None,
        "65",
    ),
    _CREATE_GX.format("select (select nosuch)"): _missing_column("73"),
    _CREATE_GX.format("update fe set nosuch = 1 returning 1"): (
        "42703",
        'column "nosuch" of relation "fe" does not exist',
        None,
        "72",
    ),
    _CREATE_GX.format("create table t9 (a int); select a from t9"): (
        "42P01",
        'relation "t9" does not exist',
        None,
        "97",
    ),
    "create procedure gxp(p int) language sql as $$ select nosuch $$": _missing_column("55"),
    _CREATE_GX.format("select case when p <= 1 then 1 else p * gx(p - 1) end"): ("rowcount", -1),
    "create function gx(p anyelement) returns int language sql as $$ select nosuch $$": (
        "rowcount",
        -1,
    ),
    "create function gx(p int) returns void language sql as $$ drop table nosuch $$": (
        "rowcount",
        -1,
    ),
    _CREATE_GX.format("select 1/0"): ("rowcount", -1),
    "set check_function_bodies = off; " + _CREATE_GX.format("select nosuch"): ("rowcount", -1),
}


@pytest.mark.backlog_fixed("RP-465")
@pytest.mark.parametrize("query", list(CREATE_CHECKS_MORE))
def test_create_checks_more_bodies(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.execute("drop function if exists gx(anyelement)")
        c.execute("drop procedure if exists gxp(int)")
        c.execute("drop table if exists t9")
        try:
            last = query
            if query.startswith("set "):
                setting, last = query.split("; ", 1)
                c.execute(setting)
            c.execute(last)
            got: Any = ("rowcount", -1)
        except psycopg.Error as e:
            got = (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )
        finally:
            c.execute("drop function if exists gx(int)")
            c.execute("drop function if exists gx(anyelement)")
            c.execute("drop procedure if exists gxp(int)")
            c.execute("drop table if exists t9")
    assert got == CREATE_CHECKS_MORE[query]


@pytest.mark.backlog_fixed("RP-465")
def test_refused_create_leaves_no_routine(dsn: str) -> None:
    """A refused CREATE left its row in `pg_proc`, and a refused OR REPLACE
    lost the routine it replaced; measured on the same cluster."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop function if exists gr()")
        c.execute("drop function if exists gnew()")
        c.execute("create function gr() returns int language sql as $$ select 1 $$")
        bad = "returns int language sql as $$ select nosuch $$"
        refused = []
        for q in (f"create or replace function gr() {bad}", f"create function gnew() {bad}"):
            try:
                c.execute(q)
            except psycopg.Error as e:
                refused.append(e.sqlstate)
        got = (
            refused,
            c.execute("select gr()").fetchall(),
            c.execute("select count(*) from pg_proc where proname = 'gnew'").fetchall(),
        )
        c.execute("drop function if exists gr()")
    assert got == (["42703", "42703"], [(1,)], [(0,)])


@pytest.mark.backlog_fixed("RP-465")
def test_create_runs_nothing_in_the_body(dsn: str) -> None:
    """Checking a body plans it, and planning answers a function in FROM by
    calling it: the check must not call it, draw a sequence or run a
    volatile subquery. Measured on the same cluster: no row, no value
    drawn."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop function if exists gw2(), gw1()")
        c.execute("drop table if exists tw")
        c.execute("drop sequence if exists sw")
        c.execute("create table tw (a int)")
        c.execute("create sequence sw")
        c.execute(
            "create function gw1() returns int language sql"
            " as $$ insert into tw values (7); select 1 $$"
        )
        c.execute(
            "create function gw2() returns bigint language sql as $$ select count(*)"
            " from tw, gw1() where (select nextval('sw')) > 0 $$"
        )
        got = (
            c.execute("select count(*) from tw").fetchall(),
            c.execute("select last_value, is_called from sw").fetchall(),
        )
        c.execute("drop function if exists gw2(), gw1()")
        c.execute("drop table if exists tw")
        c.execute("drop sequence if exists sw")
    assert got == ([(0,)], [(1, False)])


# Found while working RP-465: what PostgreSQL 15.19's validator also
# refuses at CREATE, measured on the same cluster. A pair is `(setting,
# CREATE)`; the setting runs first when there is one. Each CREATE succeeded
# here (RP-500): the body's string constants are now read by their cast's
# or their operator's type, a CALL's procedure is resolved, the final
# statement is held to the declared result (`check_sql_fn_retval`), and a
# SQL-standard body is analysed with `check_function_bodies` off.
CREATE_CHECKS_LITERALS_AND_RESULT = {
    (None, _CREATE_GX.format("select 'x'::int")): (
        "22P02",
        'invalid input syntax for type integer: "x"',
        None,
        "65",
    ),
    (None, _CREATE_GX.format("select 'a' = 1")): (
        "22P02",
        'invalid input syntax for type integer: "a"',
        None,
        "65",
    ),
    (None, _CREATE_GX.format("select 'abc'")): (
        "42P13",
        "return type mismatch in function declared to return integer",
        None,
        None,
    ),
    (None, _CREATE_GX.format("select 1, 2")): (
        "42P13",
        "return type mismatch in function declared to return integer",
        None,
        None,
    ),
    (None, "create function gc1() returns void language sql as $$ call nosuchproc() $$"): (
        "42883",
        "procedure nosuchproc() does not exist",
        "No procedure matches the given name and argument types."
        " You might need to add explicit type casts.",
        "60",
    ),
    (
        "set check_function_bodies = off",
        "create function gc2() returns int begin atomic select nosuch; end",
    ): _missing_column("55"),
}


@pytest.mark.backlog_fixed("RP-500")
@pytest.mark.parametrize("case", list(CREATE_CHECKS_LITERALS_AND_RESULT))
def test_create_checks_literals_and_result(case: tuple[str | None, str], dsn: str) -> None:
    setting, create = case
    with psycopg.connect(dsn, autocommit=True) as c:
        for f in ("gx(int)", "gc1()", "gc2()"):
            c.execute(f"drop function if exists {f}")
        if setting:
            c.execute(setting)
        try:
            c.execute(create)
            got: Any = ("rowcount", -1)
        except psycopg.Error as e:
            got = (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )
        finally:
            for f in ("gx(int)", "gc1()", "gc2()"):
                c.execute(f"drop function if exists {f}")
    assert got == CREATE_CHECKS_LITERALS_AND_RESULT[case]


# RP-500's regression: `fmgr_sql_validator`'s checks around the gap test's
# cases, measured on a private, freshly initdb'd PostgreSQL 15.19 cluster
# after `_SETUP` and `_VALIDATOR_SETUP`. An answer is "ok" (CREATE succeeds)
# or `(sqlstate, message, detail, hint, context, position)`. Bodies
# PostgreSQL accepts must still be created: a coercion the result check
# allows, a constant no input function refuses, a cast's typmod (applied
# when the expression runs, not at CREATE).
_VALIDATOR_SETUP = [
    "drop procedure if exists gvp(int)",
    "drop type if exists gvk",
    "create type gvk as (k int, l text)",
    "create procedure gvp(x int) language sql as $$ select 1 $$",
]


def _mismatch(declared: str, detail: str) -> tuple[Any, ...]:
    return (
        "42P13",
        f"return type mismatch in function declared to return {declared}",
        detail,
        None,
        'SQL function "gx"',
        None,
    )


def _input(message: str, position: str) -> tuple[Any, ...]:
    return ("22P02", message, None, None, None, position)


_GX = "create function gx(p int) returns {} language sql as $$ {} $$"
_NOT_A_QUERY = "Function's final statement must be SELECT or INSERT/UPDATE/DELETE RETURNING."
_NO_PROCEDURE_HINT = (
    "No procedure matches the given name and argument types."
    " You might need to add explicit type casts."
)

VALIDATOR_CHECKS = {
    # The final statement's result against the declared type.
    (None, _GX.format("int", "select '5'")): _mismatch("integer", "Actual return type is text."),
    (None, _GX.format("int", "select true")): _mismatch(
        "integer", "Actual return type is boolean."
    ),
    (None, _GX.format("boolean", "select 1")): _mismatch(
        "boolean", "Actual return type is integer."
    ),
    (None, _GX.format("int", "select null")): _mismatch("integer", "Actual return type is text."),
    (None, _GX.format("int", "select 1; select 'a'::text")): _mismatch(
        "integer", "Actual return type is text."
    ),
    (None, _GX.format("int", "insert into fe values (1)")): _mismatch("integer", _NOT_A_QUERY),
    (None, _GX.format("int", "create table gvz (a int)")): _mismatch("integer", _NOT_A_QUERY),
    (None, _GX.format("int", "call gvp(1)")): _mismatch("integer", _NOT_A_QUERY),
    (None, _GX.format("int", "insert into fe values (1) returning b")): _mismatch(
        "integer", "Actual return type is text."
    ),
    (None, _GX.format("int", "select 1 as a, 2 as b order by 1")): _mismatch(
        "integer", "Final statement must return exactly one column."
    ),
    (None, _GX.format("setof int", "select 'a'::text")): _mismatch(
        "integer", "Actual return type is text."
    ),
    (None, _GX.format("table (x int)", "select 'a'::text")): _mismatch(
        "integer", "Actual return type is text."
    ),
    (None, _GX.format("table (x int, y text)", "select 1")): _mismatch(
        "record", "Final statement returns too few columns."
    ),
    (None, _GX.format("gvk", "select 1")): _mismatch(
        "gvk", "Final statement returns too few columns."
    ),
    (None, _GX.format("gvk", "select 1, 'a', 2")): _mismatch(
        "gvk", "Final statement returns too many columns."
    ),
    (None, _GX.format("fe", "select a from fe")): _mismatch(
        "fe", "Final statement returns too few columns."
    ),
    (
        None,
        "create function gx(p int, out x int, out y int) language sql as $$ select 1, 'a'::text $$",
    ): _mismatch("record", "Final statement returns text instead of integer at column 2."),
    # What the result check lets through.
    (None, _GX.format("text", "select 1")): "ok",
    (None, _GX.format("bigint", "select 1")): "ok",
    (None, _GX.format("int", "select 1.5")): "ok",
    (None, _GX.format("int", "select 1::bigint")): "ok",
    (None, _GX.format("void", "select 'a'::text")): "ok",
    (None, _GX.format("int", "select 'a'::text; select 1")): "ok",
    (None, _GX.format("int", "insert into fe values (p, 'a') returning a")): "ok",
    (None, _GX.format("int", "update fe set a = p returning a")): "ok",
    (None, _GX.format("int", "values (1)")): "ok",
    (None, _GX.format("int", "select 1 union select 2")): "ok",
    (None, _GX.format("table (x int, y text)", "select 1, 2")): "ok",
    (None, _GX.format("gvk", "select 1, 'a'")): "ok",
    (None, _GX.format("gvk", "select row(1, 'a')::gvk")): "ok",
    (None, _GX.format("setof fe", "select * from fe")): "ok",
    (None, _GX.format("fe", "select f from fe f")): "ok",
    (None, _GX.format("record", "select 1, 'a'")): "ok",
    (None, _GX.format("int[]", "select array[1, 2]")): "ok",
    (None, _GX.format("text[]", "select array[1, 2]")): "ok",
    (None, _GX.format("jsonb", """select '{"a": 1}'::json""")): "ok",
    (
        None,
        "create function gx(p anyelement) returns anyelement language sql"
        " as $$ select 'a'::text $$",
    ): "ok",
    # A constant's input, read while the body is analysed.
    (None, _GX.format("int", "select 'x'::int from fe")): _input(
        'invalid input syntax for type integer: "x"', "65"
    ),
    (None, _GX.format("int", "select case when false then 'x'::int end")): _input(
        'invalid input syntax for type integer: "x"', "86"
    ),
    (None, _GX.format("int", "select '99999999999'::int")): (
        "22003",
        'value "99999999999" is out of range for type integer',
        None,
        None,
        None,
        "65",
    ),
    (None, _GX.format("int", "select a from fe where a = 'q'")): _input(
        'invalid input syntax for type integer: "q"', "85"
    ),
    (None, _GX.format("boolean", "select 1 in ('z')")): _input(
        'invalid input syntax for type integer: "z"', "75"
    ),
    (None, _GX.format("regclass", "select 'nosuchrel'::regclass")): (
        "42P01",
        'relation "nosuchrel" does not exist',
        None,
        None,
        None,
        "70",
    ),
    # What it does not refuse: an error only evaluation raises, and a
    # typmod, which is applied when the expression runs.
    (None, _GX.format("int", "select 1/0")): "ok",
    (None, _GX.format("int", "select case when p > 0 then 'x'::text::int end")): "ok",
    (None, _GX.format("numeric", "select '123.456'::numeric(4,2)")): "ok",
    (None, _GX.format("varchar(2)", "select 'abcdef'::varchar(2)")): "ok",
    (None, _GX.format("interval", "select interval '1' hour")): "ok",
    (None, _GX.format("regclass", "select 'fe'::regclass")): "ok",
    # A CALL's procedure, resolved at CREATE.
    (None, _GX.format("void", "call nosuchproc(1, 'a')")): (
        "42883",
        "procedure nosuchproc(integer, unknown) does not exist",
        None,
        _NO_PROCEDURE_HINT,
        None,
        "64",
    ),
    (None, _GX.format("void", "call gvp(1, 2)")): (
        "42883",
        "procedure gvp(integer, integer) does not exist",
        None,
        _NO_PROCEDURE_HINT,
        None,
        "64",
    ),
    (None, _GX.format("void", "call gts()")): (
        "42809",
        "gts() is not a procedure",
        None,
        "To call a function, use SELECT.",
        None,
        "64",
    ),
    (None, _GX.format("void", "call gvp('a')")): _input(
        'invalid input syntax for type integer: "a"', "68"
    ),
    (None, _GX.format("void", "call gvp(p)")): "ok",
    (None, _GX.format("void", "call gvp(1/0)")): "ok",
    # A SQL-standard body is analysed whatever `check_function_bodies`
    # says; only its result check waits for the setting.
    (
        "set check_function_bodies = off",
        "create function gx(p int) returns int begin atomic select 'abc'; end",
    ): "ok",
    (
        "set check_function_bodies = off",
        "create function gx(p int) returns int begin atomic select 'x'::int; end",
    ): _input('invalid input syntax for type integer: "x"', "59"),
    (
        "set check_function_bodies = off",
        "create function gx(p int) returns int language sql as $$ select 'abc' $$",
    ): "ok",
    (None, "create function gx(p int) returns int begin atomic select 1, 2; end"): _mismatch(
        "integer", "Final statement must return exactly one column."
    ),
}


def validator_answer(dsn: str, case: tuple[str | None, str]) -> Any:
    setting, create = case
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in [*_SETUP, *_VALIDATOR_SETUP]:
            c.execute(q)
        c.execute("drop routine if exists gx(int)")
        c.execute("drop routine if exists gx(anyelement)")
        if setting:
            c.execute(setting)
        try:
            c.execute(create)
            return "ok"
        except psycopg.Error as e:
            d = e.diag
            return (
                e.sqlstate,
                d.message_primary,
                d.message_detail,
                d.message_hint,
                d.context,
                d.statement_position,
            )
        finally:
            c.execute("drop routine if exists gx(int)")
            c.execute("drop routine if exists gx(anyelement)")
            c.execute("drop table if exists gvz")
            c.execute("drop procedure if exists gvp(int)")
            c.execute("drop type if exists gvk")


@pytest.mark.backlog_fixed("RP-500")
@pytest.mark.parametrize("case", list(VALIDATOR_CHECKS))
def test_validator_checks(case: tuple[str | None, str], dsn: str) -> None:
    assert validator_answer(dsn, case) == VALIDATOR_CHECKS[case]


# Found while working RP-465: an array parameter's subscript, measured on
# the same cluster. The parameter was bound as `$1::int4[]`, so `v[1]` read
# as a cast to an array type with a bound (22P02, or 0A000 under `+`); the
# cast is now parenthesised before a subscript (RP-502).
ARRAY_PARAMETER_SUBSCRIPT = {
    "create function h1(v int[]) returns int language sql as $$ select v[1] $$": [(4,)],
    "create function h1(v int[]) returns int language sql as $$ select v[1] + 1 $$": [(5,)],
    "create function h1(v int[]) returns int language sql"
    " as $$ select v[2] + array_length(v, 1) $$": [(7,)],
}


@pytest.mark.backlog_fixed("RP-502")
@pytest.mark.parametrize("create", list(ARRAY_PARAMETER_SUBSCRIPT))
def test_array_parameter_subscript(create: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop function if exists h1(int[])")
        c.execute(create)
        try:
            got: Any = c.execute("select h1(array[4,5])").fetchall()
        except psycopg.Error as e:
            got = (e.sqlstate, e.diag.message_primary)
        finally:
            c.execute("drop function if exists h1(int[])")
    assert got == ARRAY_PARAMETER_SUBSCRIPT[create]


# Found while working RP-461: a function with one OUT parameter, or a
# RETURNS TABLE of one column, called in FROM has one column named by that
# parameter, ahead of the item's alias (`chooseScalarFunctionAlias`), and a
# composite result its type's field; it was named by the function (or the
# item's alias), so the parameter's name was 42703. A scalar result's column
# is still the alias's, WITH ORDINALITY too, and an item's whole row is its
# scalar value or its composite.
ONE_OUT_PARAMETER_COLUMN = {
    "select t from gto()": [(6,)],
    "select g.t from gto() as g": [(6,)],
    "select t from gtt()": [(5,)],
    "select * from gto() g": [(6,)],
    "select x from gtio(1) g": [(2,)],
    "select k from gtc() g": [(8,)],
    "select g from gtc() g": [("(8)",)],
    # An unnamed OUT parameter, or a scalar result: the alias.
    "select g from gtun() g": [(3,)],
    "select gtun from gtun()": [(3,)],
    "select g from gts() g": [(4,)],
    "select g, ordinality from gts() with ordinality g": [(4, 1)],
    "select t, ordinality from gto() with ordinality g": [(6, 1)],
    "select t from rows from (gto()) g": [(6,)],
    # In a join, and LATERAL.
    "select a, t from fe, gto() g": [(1, 6)],
    "select t from fe join gtt() g on true": [(5,)],
    "select a, x from fe, lateral gtio(fe.a) g": [(1, 2)],
    "select a, value::text from fe, json_array_elements('[1]') x": [(1, "1")],
    # The item's whole row: its scalar value, NULL where an outer join
    # NULL-extends it.
    "select g from gto() g": [(6,)],
    "select gto from gto()": [(6,)],
    "select g from gtt() g where g = 5": [(5,)],
    "select g from fe, gto() g": [(6,)],
    "select g from fe left join gto() g on false": [(None,)],
}


@pytest.mark.backlog_fixed("RP-477")
@pytest.mark.parametrize("query", list(ONE_OUT_PARAMETER_COLUMN))
def test_one_out_parameter_names_the_column(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ONE_OUT_PARAMETER_COLUMN[query]


# The item's whole row in a join: a bare alias on a FROM function that names
# its own one column (`jsonb_array_elements` is `value`) is the function's
# whole-row Var, not a column. The answers were measured on PostgreSQL 15.19
# over VALUES lists, so nothing was created there: a scalar function's row is
# its value (`pg_typeof(e)` is jsonb), WITH ORDINALITY's and a subquery's a
# record, and a LEFT JOIN NULL-extends either to NULL. LATERAL, the implicit
# lateral of a comma join, and a function either side of a plain JOIN all
# read it; `e.value` and a bare `value` still read the column. The two user
# functions follow from the same rule (one named OUT parameter: a scalar).
_T = "(VALUES (1, '[1,2]'::jsonb)) t(id, j)"
_T2 = "(VALUES (1, '[1,2]'::jsonb), (2, '[]'::jsonb)) t(id, j)"
JOINED_FUNCTION_WHOLE_ROW = {
    f"SELECT t.id, e FROM {_T}, LATERAL jsonb_array_elements(t.j) AS e": [(1, 1), (1, 2)],
    f"SELECT t.id, e FROM {_T}, jsonb_array_elements(t.j) AS e": [(1, 1), (1, 2)],
    f"SELECT t.id, e FROM {_T}, LATERAL json_array_elements(t.j::json) AS e": [(1, 1), (1, 2)],
    'SELECT t.id, e FROM (VALUES (1, \'["a","b"]\'::jsonb)) t(id, j)'
    " CROSS JOIN LATERAL jsonb_array_elements_text(t.j) AS e": [(1, "a"), (1, "b")],
    f"SELECT t.id, e FROM {_T2} LEFT JOIN LATERAL jsonb_array_elements(t.j) AS e ON true"
    " ORDER BY 1, 2": [(1, 1), (1, 2), (2, None)],
    f"SELECT t.id, e FROM {_T} LEFT JOIN LATERAL jsonb_array_elements(t.j) AS e ON false": [
        (1, None)
    ],
    f"SELECT t.id, e, e IS NULL FROM {_T2} LEFT JOIN LATERAL jsonb_array_elements(t.j) AS e"
    " ON true ORDER BY 1, 2": [(1, 1, False), (1, 2, False), (2, None, True)],
    f"SELECT t.id, e FROM {_T} JOIN LATERAL jsonb_array_elements(t.j) AS e ON e = '2'::jsonb": [
        (1, 2)
    ],
    f"SELECT t.id FROM {_T}, LATERAL jsonb_array_elements(t.j) AS e WHERE e = '2'": [(1,)],
    # The column keeps the function's name.
    f"SELECT t.id, e.value, pg_typeof(e) FROM {_T}, LATERAL jsonb_array_elements(t.j) AS e": [
        (1, 1, "jsonb"),
        (1, 2, "jsonb"),
    ],
    f"SELECT t.id, value FROM {_T}, LATERAL jsonb_array_elements(t.j) AS e": [(1, 1), (1, 2)],
    f"SELECT t.id, e.* FROM {_T}, LATERAL jsonb_array_elements(t.j) AS e": [(1, 1), (1, 2)],
    f"SELECT t.id, e, v FROM {_T}, LATERAL jsonb_array_elements(t.j) AS e(v)": [
        (1, 1, 1),
        (1, 2, 2),
    ],
    # A column of the alias's name wins over the row.
    f"SELECT t.id, value FROM {_T}, LATERAL jsonb_array_elements(t.j) AS value": [
        (1, 1),
        (1, 2),
    ],
    # The function on either side of a plain JOIN, not lateral.
    "SELECT e FROM (VALUES (1)) t(id) JOIN jsonb_array_elements('[1,2]') AS e ON true": [
        (1,),
        (2,),
    ],
    "SELECT e FROM jsonb_array_elements('[1,2]') AS e, (VALUES (1)) t(id)": [(1,), (2,)],
    "SELECT e, t.id FROM jsonb_array_elements('[1,2]') AS e"
    " RIGHT JOIN (VALUES (1), (2), (3)) t(id) ON e::text = t.id::text ORDER BY 2": [
        (1, 1),
        (2, 2),
        (None, 3),
    ],
    # WITH ORDINALITY: the row is a record of value and ordinality.
    f"SELECT t.id, e FROM {_T}, LATERAL jsonb_array_elements(t.j) WITH ORDINALITY AS e": [
        (1, ("1", "1")),
        (1, ("2", "2")),
    ],
    f"SELECT t.id, e FROM {_T2} LEFT JOIN LATERAL jsonb_array_elements(t.j) WITH ORDINALITY AS e"
    " ON true ORDER BY 1": [(1, ("1", "1")), (1, ("2", "2")), (2, None)],
    # A later FROM function reads the row as its argument.
    "SELECT t.id, e, f FROM (VALUES (1, '[[1],[2]]'::jsonb)) t(id, j),"
    " LATERAL jsonb_array_elements(t.j) AS e, LATERAL jsonb_array_elements(e) AS f": [
        (1, [1], 1),
        (1, [2], 2),
    ],
    # A LATERAL subquery's row is a record, NULL where an outer join
    # NULL-extends it.
    f"SELECT t.id, e FROM {_T}, LATERAL (SELECT x.value, t.id AS i2"
    " FROM jsonb_array_elements(t.j) x) e": [(1, ("1", "1")), (1, ("2", "1"))],
    f"SELECT t.id, e FROM {_T} LEFT JOIN LATERAL (SELECT x.value"
    " FROM jsonb_array_elements(t.j) x WHERE x.value = '3') e ON true": [(1, None)],
    # A user function with one named OUT parameter.
    "select a, g from fe, lateral gtio(fe.a) g": [(1, 2)],
    "select a, g from fe left join lateral gtio(fe.a) g on false": [(1, None)],
}


@pytest.mark.backlog_fixed("RP-477")
@pytest.mark.parametrize("query", list(JOINED_FUNCTION_WHOLE_ROW))
def test_joined_function_names_its_whole_row(query: str, dsn: str) -> None:
    assert answer(dsn, query) == JOINED_FUNCTION_WHOLE_ROW[query]


# Found while working RP-477, fixed by RP-483: in a select list a call is
# named by its function (FigureColname), and the result of one with a
# single OUT parameter is a scalar, so a field of it is 42809.
ONE_OUT_PARAMETER_CALL = {
    "select gtt()": [(5,)],
    "select (gto()).t": (
        "42809",
        "column notation .t applied to type integer, which is not a composite type",
        None,
        "9",
    ),
}


@pytest.mark.backlog_fixed("RP-483")
@pytest.mark.parametrize("query", list(ONE_OUT_PARAMETER_CALL))
def test_one_out_parameter_call_in_the_select_list(query: str, dsn: str) -> None:
    answer_ = answer(dsn, query)
    if query == "select gtt()":
        # The column's name was the divergence: the value already agreed.
        assert column_names(dsn, query) == ["gtt"]
    assert answer_ == ONE_OUT_PARAMETER_CALL[query]


def column_names(dsn: str, query: str) -> list[str]:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        cur = c.execute(query)
        return [d.name for d in cur.description]


def _notation(field: str, ty: str, at: str) -> tuple[str, str, None, str]:
    return (
        "42809",
        f"column notation .{field} applied to type {ty}, which is not a composite type",
        None,
        at,
    )


# RP-483's further shapes, measured on the same private 15.19 cluster: the
# one-column set-returning call is a scalar inside an expression too, and
# column notation over ANY base type is refused at analysis, wherever it is
# written and whatever the rows (a NULL included). Each answered 0A000, a
# record or the field's value before.
COLUMN_NOTATION = {
    "select gtt() + 1": [(6,)],
    "select gtt()::text": [("5",)],
    "select gtt from (select gtt()) s": [(5,)],
    "select (gtt()).t": _notation("t", "integer", "9"),
    "select (gtio(1)).x": _notation("x", "integer", "9"),
    "select (gto()).*": ("42809", "type integer is not composite", None, None),
    "select (gtt()).*": ("42809", "type integer is not composite", None, None),
    "select (1).x": _notation("x", "integer", "9"),
    "select (null::int).x": _notation("x", "integer", "9"),
    "select ('a'::text).x": _notation("x", "text", "9"),
    "select (array[array[1]]).x": _notation("x", "integer[]", "9"),
    "select (now()).x": _notation("x", "timestamp with time zone", "9"),
    "select (a).x from fe": _notation("x", "integer", "9"),
    "select (fe.a).x from fe": _notation("x", "integer", "9"),
    "select 1 from fe where (a).x = 1": _notation("x", "integer", "25"),
    "select (generate_series(1, 2)).x": _notation("x", "integer", "9"),
    "select (unnest(array[1])).x": _notation("x", "integer", "9"),
    "select (json_array_elements('[1]')).value": _notation("value", "json", "9"),
    # A composite's field still reads.
    "select (gtc()).k": [(8,)],
}


@pytest.mark.backlog_fixed("RP-483")
@pytest.mark.parametrize("query", list(COLUMN_NOTATION))
def test_column_notation_on_a_base_type(query: str, dsn: str) -> None:
    assert answer(dsn, query) == COLUMN_NOTATION[query]


# A set-returning built-in in a select list is named by the function, not
# by its FROM column (`value`); measured on the same cluster (RP-483).
SRF_TARGET_NAME = {
    "select json_array_elements('[1,2]')": ["json_array_elements"],
    "select json_array_elements_text('[1,2]')": ["json_array_elements_text"],
    "select jsonb_array_elements('[1]')": ["jsonb_array_elements"],
    "select gtt() as z": ["z"],
}


@pytest.mark.backlog_fixed("RP-483")
@pytest.mark.parametrize("query", list(SRF_TARGET_NAME))
def test_set_returning_target_is_named_by_its_function(query: str, dsn: str) -> None:
    assert column_names(dsn, query) == SRF_TARGET_NAME[query]


# Found while working RP-483: a call qualified by a schema that does not
# exist is 3F000 on PostgreSQL 15.19 (`LookupExplicitNamespace`), at the
# call; `pg_temp` is such a schema until the session makes a temp object.
MISSING_SCHEMA_CALL = {
    "select nosuch_schema.nosuch()": (
        "3F000",
        'schema "nosuch_schema" does not exist',
        None,
        "8",
    ),
    "select 1 + nosuch_schema.f(1)": (
        "3F000",
        'schema "nosuch_schema" does not exist',
        None,
        "12",
    ),
    "select pg_temp.nosuch()": ("3F000", 'schema "pg_temp" does not exist', None, "8"),
}


@pytest.mark.backlog("RP-485")
@pytest.mark.parametrize("query", list(MISSING_SCHEMA_CALL))
def test_call_in_a_missing_schema(query: str, dsn: str) -> None:
    assert answer(dsn, query) == MISSING_SCHEMA_CALL[query]
