"""Gap tests of the function-body divergences found while working RP-451:
RP-461 (a SQL function's bare parameter name was replaced wherever it was
a word; fixed), RP-462 (a composite parameter's field was not read; fixed), RP-463 (a
PL/pgSQL block label does not qualify its variables), RP-464 (a SQL
function ending in a write's RETURNING returned NULL; fixed) and RP-465 (CREATE
FUNCTION does not check a SQL body); and found while working RP-461,
RP-477 (a function with one OUT parameter, called in FROM, named its
column after the function; fixed), found while working RP-477, RP-483
(the same function's call in a select list was named after its parameter,
and a field of its scalar result was not refused; fixed), and found while
working RP-483, RP-485 (a call in a schema that does not exist is a missing
function, not a missing schema).

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
