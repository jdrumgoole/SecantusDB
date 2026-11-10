"""Gap tests of divergences found while working RP-711 (a PL/pgSQL
assignment to an array element was refused):

- an array literal whose upper bound passes `int4` is accepted, where
  `array_in`'s `ArrayCheckBounds` refuses it;
- jsonb subscripting -- a fetch, an UPDATE's `SET j['k'] = v`, a PL/pgSQL
  `x['k'] := v` -- is refused, the first two with a 42804 that says jsonb
  does not support subscripting.

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
    "select ('[2147483646:2147483646]={1}'::int[])::text": (
        "ok",
        [("[2147483646:2147483646]={1}",)],
        [],
    ),
}


@pytest.mark.backlog("RP-771")
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


@pytest.mark.backlog("RP-772")
@pytest.mark.parametrize("query", list(JSONB_SUBSCRIPTS))
def test_jsonb_subscripting(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == JSONB_SUBSCRIPTS[query]
