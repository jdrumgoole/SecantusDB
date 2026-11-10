"""Gap tests of divergences found while working RP-695 (a PL/pgSQL
assignment to a `varchar(n)[]` variable truncated an overlong element):

- an INSERT or UPDATE of a `varchar(n)[]`, `char(n)[]` or `bit(n)[]`
  column fitted no element to the column's modifier, where PostgreSQL's
  `ArrayCoerceExpr` applies the element's length coercion by the
  assignment rule (fixed by RP-710);
- a PL/pgSQL assignment to an array element, `x[1] := v`, was refused,
  the element's value cast to the whole array's type (fixed by RP-711).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP` (the `u711` UPDATE cases with
`_UPDATE_SETUP`); the DO blocks, which create nothing, on the shared 15.19
reference too.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t695",
    "create table t695 (k int, a varchar(3)[], c char(3)[], b bit(3)[])",
    "insert into t695 (k) values (1)",
]
_VARCHAR3 = ("error", "22001", "value too long for type character varying(3)")


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _answer(dsn: str, query: str) -> object:
    notes: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.add_notice_handler(lambda d: notes.append(d.message_primary))
        try:
            cur = c.execute(query)
            rows = cur.fetchall() if cur.description else None
            return ("ok", rows, notes)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


# `transformAssignedExpr` coerces the value to the column's type and
# modifier by the assignment rule; for an array that is an `ArrayCoerceExpr`
# over the element's length coercion function.
ARRAY_COLUMN = {
    "insert into t695 (a) values (array['abcdef'])": _VARCHAR3,
    "insert into t695 (a) values ('{abcdef}')": _VARCHAR3,
    "update t695 set a = array['ab', 'abcdef']": _VARCHAR3,
    "insert into t695 (c) values (array['abcd'])": (
        "error",
        "22001",
        "value too long for type character(3)",
    ),
    "insert into t695 (b) values (array[B'1'])": (
        "error",
        "22026",
        "bit string length 1 does not match type bit(3)",
    ),
    "insert into t695 (k, a) values (2, array['abc   ']) returning a": ("ok", [(["abc"],)], []),
}


@pytest.mark.backlog_fixed("RP-710")
@pytest.mark.parametrize("query", list(ARRAY_COLUMN))
def test_array_column_assignment_fits_each_element(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ARRAY_COLUMN[query]


# PostgreSQL 15 compiles `x[i] := v` into one `SubscriptingRef` over the
# variable (`transformPLAssignStmt`): each subscript is coerced to `int4` by
# the assignment rule and checked (a NULL one is 22004) before the value,
# which is coerced to the element type (the array type for a slice), and
# `array_set_element` / `array_set_slice` place it. A one-dimensional array
# grows to meet a subscript past either end, a multidimensional one does
# not, and assigning into a NULL array builds one at the subscripts given.
_SIZE = ("error", "54000", "array size exceeds the maximum allowed (134217727)")
_SUBSCRIPT_TYPE = ("error", "42804", "array subscript must have type integer")
_TOO_SMALL = ("error", "2202E", "source array too small")
_WRONG_NUMBER = ("error", "2202E", "wrong number of array subscripts")
_OUT_OF_RANGE = ("error", "2202E", "array subscript out of range")


def _do(decl: str, body: str = "raise notice '%', x;") -> str:
    return f"do $$ declare {decl}; begin {body} end $$"


def _shows(*notes: str) -> tuple[str, None, list[str]]:
    return ("ok", None, list(notes))


ELEMENT_ASSIGNMENT = {
    "do $$ declare x int[]; begin x[1] := 5; raise notice '%', x; end $$": (
        "ok",
        None,
        ["{5}"],
    ),
    "do $$ declare x text[] := '{a}'; begin x[2] := 'ab'; raise notice '%', x; end $$": (
        "ok",
        None,
        ["{a,ab}"],
    ),
    "do $$ declare x varchar(3)[]; begin x[1] := 'abcdef'; end $$": _VARCHAR3,
    # Growth, and the bounds an assignment moves.
    _do("x int[] := '{1,2}'", "x[4] := 5; raise notice '%', x;"): _shows("{1,2,NULL,5}"),
    _do("x int[] := '{1,2}'", "x[0] := 9; raise notice '%', x;"): _shows("[0:2]={9,1,2}"),
    _do("x int[]; i int := 3", "x[i+1] := i; raise notice '%', x;"): _shows("[4:4]={3}"),
    _do("x int[] := '[0:1]={1,2}'", "x[1] := 5; raise notice '%', x;"): _shows("[0:1]={1,5}"),
    _do("x int[] := '{1,2}'", "x[array_length(x,1)+1] := 7; raise notice '%', x;"): _shows(
        "{1,2,7}"
    ),
    _do("x int[]", "for i in 1..3 loop x[i] := i*i; end loop; raise notice '%', x;"): _shows(
        "{1,4,9}"
    ),
    _do('"X" int[]', '"X"[2] := 5; raise notice \'%\', "X";'): _shows("[2:2]={5}"),
    _do("x int[]", "x [ 1 ] := 5; raise notice '%', x;"): _shows("{5}"),
    _do("x int[]", "x[(select 2)] := 5; raise notice '%', x;"): _shows("[2:2]={5}"),
    _do("x int[] := '{1}'", "x[3] = 3; raise notice '%', x;"): _shows("{1,NULL,3}"),
    _do("x int[] := '{1}'", "x[1] := null; raise notice '%', x;"): _shows("{NULL}"),
    _do("x text[] := '{a,b,c}'", "x[2] := x[1] || x[3]; raise notice '%', x;"): _shows("{a,ac,c}"),
    # Several dimensions: no growth.
    _do("x int[] := '{{1,2},{3,4}}'", "x[2][1] := 9; raise notice '%', x;"): _shows(
        "{{1,2},{9,4}}"
    ),
    _do("x int[]", "x[1][2] := 5; raise notice '%', x;"): _shows("[1:1][2:2]={{5}}"),
    _do("x int[][]", "x[1][1] := 4; raise notice '%', x;"): _shows("{{4}}"),
    _do("x int[] := '[0:1][0:1]={{1,2},{3,4}}'", "x[1][0] := 5; raise notice '%', x;"): _shows(
        "[0:1][0:1]={{1,2},{5,4}}"
    ),
    _do("x int[] := '{{1,2},{3,4}}'", "x[3][1] := 9;"): _OUT_OF_RANGE,
    _do("x int[] := '{{1,2},{3,4}}'", "x[1] := 9;"): _WRONG_NUMBER,
    _do("x int[] := '{1,2}'", "x[1][1] := 9;"): _WRONG_NUMBER,
    # The value, coerced to the element type by the assignment rule.
    _do("x int[] := '{1}'", "x[1] := 2.7; raise notice '%', x;"): _shows("{3}"),
    _do("x int[] := '{1}'", "x[1] := '7'::text; raise notice '%', x;"): _shows("{7}"),
    _do("x text[] := '{a}'", "x[1] := 42; raise notice '%', x;"): _shows("{42}"),
    _do("x char(3)[]", "x[1] := 'ab'; raise notice '%', octet_length(x[1]);"): _shows("3"),
    _do("x varchar(3)[]", "x[1] := 'abc   '; raise notice '%', x;"): _shows("{abc}"),
    _do("x numeric(4,1)[]", "x[1] := 2.25; raise notice '%', x;"): _shows("{2.3}"),
    _do("x int[] := '{1}'", "x[1] := 'abc';"): (
        "error",
        "22P02",
        'invalid input syntax for type integer: "abc"',
    ),
    # The subscripts: `int4` by the assignment rule, never NULL, checked
    # before the value unless the value folds while the assignment is
    # planned.
    _do("x int[] := '{1}'", "x[null] := 1;"): (
        "error",
        "22004",
        "array subscript in assignment must not be null",
    ),
    _do("x int[]; z int := 0", "x[null] := 1/z;"): (
        "error",
        "22004",
        "array subscript in assignment must not be null",
    ),
    _do("x int[]", "x[null] := 1/0;"): ("error", "22012", "division by zero"),
    _do("x int[] := '{1}'", "x['2'] := 5; raise notice '%', x;"): _shows("{1,5}"),
    _do("x int[] := '{1}'", "x[1.6] := 5; raise notice '%', x;"): _shows("{1,5}"),
    _do("x int[]", "x[1.5::float8] := 1; raise notice '%', x;"): _shows("[2:2]={1}"),
    _do("x int[]", "x['1'::text] := 1;"): _SUBSCRIPT_TYPE,
    _do("x int[]", "x[true] := 1;"): _SUBSCRIPT_TYPE,
    _do("x int[] := '{1}'", "x[3000000000] := 1;"): ("error", "22003", "integer out of range"),
    _do("x int[] := '{1}'", "x[1000000000] := 1;"): _SIZE,
    _do("x int[]", "x[2147483647] := 1;"): (
        "error",
        "54000",
        "array lower bound is too large: 2147483647",
    ),
    _do("x int", "x[1] := 5;"): (
        "error",
        "42804",
        "cannot subscript type integer because it does not support subscripting",
    ),
    # A record's field (`r.a[2] := 5`).
    _do(
        "r record",
        "select 1 as k, '{1}'::int[] as a into r; r.a[2] := 5; raise notice '%', r;",
    ): _shows('(1,"{1,5}")'),
    _do("r record", "r.a[1] := 1;"): ("error", "55000", 'record "r" is not assigned yet'),
    _do("r record", "select 1 as k into r; r.k[1] := 1;"): (
        "error",
        "42804",
        "cannot subscript type integer because it does not support subscripting",
    ),
    # Slices.
    _do("x int[] := '{1,2,3}'", "x[2:3] := '{8,9}'; raise notice '%', x;"): _shows("{1,8,9}"),
    _do("x int[] := '{1,2,3}'", "x[2:] := array[7,8]; raise notice '%', x;"): _shows("{1,7,8}"),
    _do("x int[] := '{1,2,3}'", "x[:2] := array[7,8]; raise notice '%', x;"): _shows("{7,8,3}"),
    _do("x int[] := '{1,2,3}'", "x[0:1] := '{{7,8}}'; raise notice '%', x;"): _shows(
        "[0:3]={7,8,2,3}"
    ),
    _do("x int[] := '{1,2,3}'", "x[2:3] := null; raise notice '%', x;"): _shows("{1,2,3}"),
    _do("x int[]", "x[1:2] := null; raise notice '%', coalesce(x::text, 'NULL');"): _shows("{}"),
    _do("x int[]", "x[2:3] := '{8,9,10}'; raise notice '%', x;"): _shows("[2:3]={8,9}"),
    _do("x int[]", "x[3:2] := '{8}'; raise notice '%', x;"): _shows("{}"),
    _do("x int[]", "x[1:2][3:3] := '{5,6}'; raise notice '%', x;"): _shows("[1:2][3:3]={{5},{6}}"),
    _do("x int[] := '{{1,2},{3,4}}'", "x[1:1][1:2] := '{{8,9}}'; raise notice '%', x;"): _shows(
        "{{8,9},{3,4}}"
    ),
    _do("x int[] := '{{1,2},{3,4}}'", "x[2:2] := '{8,9}'; raise notice '%', x;"): _shows(
        "{{1,2},{8,9}}"
    ),
    _do("x int[] := '{1,2,3}'", "x[1:2] := '{9}';"): _TOO_SMALL,
    _do("x int[] := '{{1,2},{3,4}}'", "x[2][1:2] := '{8,9}';"): _TOO_SMALL,
    _do("x int[] := '{{1,2},{3,4}}'", "x[1:3][1] := '{8,9,7}';"): _OUT_OF_RANGE,
    _do("x int[] := '{1,2,3}'", "x[3:2] := '{8,9}';"): (
        "error",
        "2202E",
        "upper bound cannot be less than lower bound",
    ),
    _do("x int[]", "x[2:] := '{8,9}';"): (
        "error",
        "2202E",
        "array slice subscript must provide both boundaries",
    ),
    _do("x int[]", "x[-2147483648:2147483647] := '{1}';"): _SIZE,
    _do("x varchar(3)[]", "x[1:1] := array['abcd'];"): _VARCHAR3,
    _do("x int[]", "x[1:2] := 'abc';"): ("error", "22P02", 'malformed array literal: "abc"'),
}


@pytest.mark.backlog_fixed("RP-711")
@pytest.mark.parametrize("query", list(ELEMENT_ASSIGNMENT))
def test_assignment_to_an_array_element(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ELEMENT_ASSIGNMENT[query]


# `UPDATE ... SET a[...] = v` places the value by the same
# `array_set_element` / `array_set_slice` (RP-711).
_UPDATE_SETUP = [
    "drop table if exists u711",
    "create table u711 (k int primary key, a int[], m int[], v varchar(3)[])",
    "insert into u711 values (1, '{1,2,3}', '{{1,2},{3,4}}', '{ab}'), (2, null, null, null)",
]
UPDATE_SUBSCRIPTS = {
    "update u711 set a[5] = 9 where k = 1 returning a::text": [("{1,2,3,NULL,9}",)],
    "update u711 set a[0] = 9 where k = 1 returning a::text": [("[0:3]={9,1,2,3}",)],
    "update u711 set a[2:3] = null where k = 1 returning a::text": [("{1,2,3}",)],
    "update u711 set a[1:2] = null where k = 2 returning a::text": [("{}",)],
    "update u711 set a[2:3] = '{8}' where k = 1 returning a": _TOO_SMALL,
    "update u711 set m[3][1] = 9 where k = 1 returning m": _OUT_OF_RANGE,
    "update u711 set m[1:1][1:2] = '{{8,9}}' where k = 1 returning m::text": [("{{8,9},{3,4}}",)],
    "update u711 set m[2:2] = '{8,9}' where k = 1 returning m::text": [("{{1,2},{8,9}}",)],
    "update u711 set m[1][2] = 5 where k = 2 returning m::text": [("[1:1][2:2]={{5}}",)],
    "update u711 set a[null] = 1 where k = 1 returning a": (
        "error",
        "22004",
        "array subscript in assignment must not be null",
    ),
    "update u711 set a[1:null] = '{1}' where k = 1 returning a": (
        "error",
        "22004",
        "array subscript in assignment must not be null",
    ),
    "update u711 set a[2147483647] = 1 where k = 1 returning a": _SIZE,
    "update u711 set a[2147483647] = 1 where k = 2 returning a": (
        "error",
        "54000",
        "array lower bound is too large: 2147483647",
    ),
    "update u711 set a[2:] = '{7,8,9}' where k = 2 returning a": (
        "error",
        "2202E",
        "array slice subscript must provide both boundaries",
    ),
    "update u711 set a[3:2] = '{7}' where k = 1 returning a": (
        "error",
        "2202E",
        "upper bound cannot be less than lower bound",
    ),
    "update u711 set v[2] = 'abcd' where k = 1 returning v": _VARCHAR3,
    "update u711 set a[1] = 1, a[1:2] = '{5,6}' where k = 1 returning a::text": [("{5,6,3}",)],
    "update u711 set m[1] = 1 where k = 1 returning m": _WRONG_NUMBER,
}


def _update_answer(dsn: str, query: str) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _UPDATE_SETUP:
            c.execute(q)
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])
        finally:
            c.execute(_UPDATE_SETUP[0])


@pytest.mark.backlog_fixed("RP-711")
@pytest.mark.parametrize("query", list(UPDATE_SUBSCRIPTS))
def test_update_of_an_array_element_or_slice(query: str, dsn: str) -> None:
    assert _update_answer(dsn, query) == UPDATE_SUBSCRIPTS[query]
