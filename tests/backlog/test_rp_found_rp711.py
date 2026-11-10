"""Gap tests of divergences found while working RP-711 (a PL/pgSQL
assignment to an array element was refused):

- an array literal whose upper bound passes `int4` was accepted, where
  `array_in`'s `ArrayCheckBounds` refuses it (fixed, RP-771);
- jsonb subscripting -- a fetch, an UPDATE's `SET j['k'] = v`, a PL/pgSQL
  `x['k'] := v` -- was refused, the first two with a 42804 that said jsonb
  does not support subscripting (fixed, RP-772: `jsonbsubs.c` ported).

The expected answers were measured on PostgreSQL 15.19: the SELECT and DO
statements on the shared reference (they create nothing), the UPDATEs on a
private, freshly initdb'd cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists j711",
    "create table j711 (k int primary key, j jsonb)",
    """insert into j711 values (1, '{"a": [1]}')""",
]


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


# `array_in` checks each dimension's `lb + length` against `int4`
# (`ArrayCheckBounds`), so an upper bound of 2147483647 is refused.
ARRAY_BOUNDS = {
    "select '[2147483647:2147483647]={1}'::int[]": (
        "error",
        "54000",
        "array lower bound is too large: 2147483647",
    ),
    "select '[2147483646:2147483647]={1,2}'::int[]": (
        "error",
        "54000",
        "array lower bound is too large: 2147483646",
    ),
    # Checked before the first element is read: `x` is never parsed.
    "select '[2147483647:2147483647]={x}'::int[]": (
        "error",
        "54000",
        "array lower bound is too large: 2147483647",
    ),
    "select ('[2147483646:2147483646]={1}'::int[])::text": (
        "ok",
        [("[2147483646:2147483646]={1}",)],
        [],
    ),
}


@pytest.mark.backlog_fixed("RP-771")
@pytest.mark.parametrize("query", list(ARRAY_BOUNDS))
def test_array_input_refuses_an_upper_bound_past_int4(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ARRAY_BOUNDS[query]


# jsonb's subscripting handler (`jsonbsubs.c`): a fetch is
# `jsonb_get_element`, an assignment `jsonb_set_element`, which builds the
# missing path and pads an array with nulls.
JSONB_SUBSCRIPTS = {
    """select ('{"a": [1,2]}'::jsonb)['a'][1]""": ("ok", [(2,)], []),
    "update j711 set j['b'] = '2' where k = 1 returning j": (
        "ok",
        [({"a": [1], "b": 2},)],
        [],
    ),
    "update j711 set j['a'][3] = '5' where k = 1 returning j": (
        "ok",
        [({"a": [1, None, None, 5]},)],
        [],
    ),
    "do $$ declare x jsonb := '{}'; begin x['a'] := '1'; raise notice '%', x; end $$": (
        "ok",
        None,
        ['{"a": 1}'],
    ),
    "do $$ declare x jsonb; begin x['a']['b'] := '1'; raise notice '%', x; end $$": (
        "ok",
        None,
        ['{"a": {"b": 1}}'],
    ),
}


@pytest.mark.backlog_fixed("RP-772")
@pytest.mark.parametrize("query", list(JSONB_SUBSCRIPTS))
def test_jsonb_subscripting(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == JSONB_SUBSCRIPTS[query]


# The rest of `jsonbsubs.c` and `setPath`: each subscript is coerced to the
# ONE of int4 and text it coerces to implicitly (a slice, a bigint, a
# numeric or a boolean is 42804); an array step is `strtoint`'s (leading
# blanks taken, trailing ones not); a NULL source is `[]` under an integer
# first subscript, `{}` otherwise; a missing path is built and an array
# padded with nulls; a negative index past the start, a step into a scalar
# and a NULL subscript are refused. Measured on PostgreSQL 15.19 (the
# UPDATEs on a private cluster with `_SETUP`).
JSONB_SUBSCRIPT_EDGES = {
    "select ('{\"a\": [1, 2]}'::jsonb)['a'][-1]": ("ok", [(2,)], []),
    "select ('[1, 2]'::jsonb)[' 1']": ("ok", [(2,)], []),
    "select ('[1, 2]'::jsonb)['1 ']": ("ok", [(None,)], []),
    "select ('{\"1\": 5}'::jsonb)[1]": ("ok", [(5,)], []),
    "select ('{\"a\": 1}'::jsonb)[null]": ("ok", [(None,)], []),
    "select ('\"x\"'::jsonb)['a']": ("ok", [(None,)], []),
    "select pg_typeof(('{\"a\": 1}'::jsonb)['a'])": ("ok", [("jsonb",)], []),
    "select ('{\"a\": 1}'::jsonb)[1.5]": (
        "error",
        "42804",
        "subscript type numeric is not supported",
    ),
    "select ('{\"a\": 1}'::jsonb)[1::int8]": (
        "error",
        "42804",
        "subscript type bigint is not supported",
    ),
    "select ('{\"a\": 1}'::jsonb)['a':'b']": (
        "error",
        "42804",
        "jsonb subscript does not support slices",
    ),
    "select (null::jsonb)[true]": ("error", "42804", "subscript type boolean is not supported"),
    "select k, j['a'][0], j['a'][k] from j711": ("ok", [(1, 1, None)], []),
    "do $$ declare x jsonb := '[1]'; begin x[5] := '2'; raise notice '%', x; end $$": (
        "ok",
        None,
        ["[1, null, null, null, null, 2]"],
    ),
    "do $$ declare x jsonb; begin x[2] := '1'; raise notice '%', x; end $$": (
        "ok",
        None,
        ["[null, null, 1]"],
    ),
    "do $$ declare x jsonb := '[]'; begin x[1]['a'][0] := '1'; raise notice '%', x; end $$": (
        "ok",
        None,
        ['[null, {"a": [1]}]'],
    ),
    "do $$ declare x jsonb := '[1]'; begin x[-5] := '2'; end $$": (
        "error",
        "22023",
        "path element at position 1 is out of range: -5",
    ),
    "do $$ declare x jsonb := '1'; begin x['a'] := '2'; end $$": (
        "error",
        "22023",
        "cannot replace existing key",
    ),
    "do $$ declare x jsonb := '{}'; begin x[null] := '1'; end $$": (
        "error",
        "22004",
        "jsonb subscript in assignment must not be null",
    ),
    "do $$ declare x jsonb := '[1]'; begin x['a'] := '1'; end $$": (
        "error",
        "22P02",
        'path element at position 1 is not an integer: "a"',
    ),
    "do $$ declare x jsonb := '[1]'; begin x[1:2] := '5'; end $$": (
        "error",
        "42804",
        "jsonb subscript does not support slices",
    ),
    "update j711 set j[k] = '1' where k = 1 returning j": ("ok", [({"1": 1, "a": [1]},)], []),
    "update j711 set j['a'] = null where k = 1 returning j": ("ok", [({"a": None},)], []),
    "update j711 set j['a'][-1] = '7' where k = 1 returning j": ("ok", [({"a": [7]},)], []),
    "update j711 set j['b']['c'][1] = '7' where k = 1 returning j": (
        "ok",
        [({"a": [1], "b": {"c": [None, 7]}},)],
        [],
    ),
    "update j711 set j['a'][null::int] = '1' where k = 1 returning j": (
        "error",
        "22004",
        "jsonb subscript in assignment must not be null",
    ),
    "update j711 set j['a'][-3] = '1' where k = 1 returning j": (
        "error",
        "22023",
        "path element at position 2 is out of range: -3",
    ),
    "update j711 set j['a']['b'] = '1' where k = 1 returning j": (
        "error",
        "22P02",
        'path element at position 2 is not an integer: "b"',
    ),
    "update j711 set j['a'][0]['x'] = '1' where k = 1 returning j": (
        "error",
        "22023",
        "cannot replace existing key",
    ),
}


@pytest.mark.backlog_fixed("RP-772")
@pytest.mark.parametrize("query", list(JSONB_SUBSCRIPT_EDGES))
def test_jsonb_subscripting_edges(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == JSONB_SUBSCRIPT_EDGES[query]
