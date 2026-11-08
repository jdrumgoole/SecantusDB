"""Gap tests of the divergences found while working RP-569 (an array built
from a domain-typed column is the domain's array):

- a cast of a domain column to its own domain checked the domain's
  constraints, so a row stored before a `NOT VALID` CHECK failed it
  (fixed, RP-655);
- an `ARRAY(select c ...)` subquery over a `char(n)` column dropped the
  column's blanks (fixed, RP-656);
- a domain column read through a join, a WITH or a set operation was
  typed by its base type in an array (fixed, RP-657).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t569nv, t569c, t569d cascade",
    "drop domain if exists d569nv, d569c cascade",
    "create domain d569nv as int",
    "create table t569nv (a d569nv)",
    "insert into t569nv values (-1), (5)",
    "alter domain d569nv add constraint d569nv_pos check (value > 0) not valid",
    "create domain d569c as char(3)",
    "create table t569c (c char(3))",
    "insert into t569c values ('xy')",
    "create table t569d (a d569c, i int)",
    "insert into t569d values ('a b', 1), ('xy', 2)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:2]:
            c.execute(q)


def _connect(dsn: str) -> psycopg.Connection:
    c = psycopg.connect(dsn, autocommit=True)
    for q in _SETUP:
        c.execute(q)
    return c


def _answer(c: psycopg.Connection, query: str) -> tuple[object, ...]:
    try:
        cur = c.execute(query)
    except psycopg.Error as e:
        return ("error", e.sqlstate, str(e).splitlines()[0])
    assert cur.description is not None
    return (cur.description[0].type_code, cur.fetchall())


def _typarray(c: psycopg.Connection, domain: str) -> int:
    row = c.execute("select typarray::int from pg_type where typname = %s", (domain,)).fetchone()
    assert row is not None
    return int(row[0])


_REGTYPE = 2206
_INT4 = 23

# A column declared with a domain is already OF the domain, so a cast to it
# is no coercion at all (`coerce_type` returns the expression as it is) and
# checks nothing: a value stored before a CHECK added NOT VALID reads back,
# under the domain's name, and in the domain's array -- in a WHERE, through
# an ARRAY or scalar subquery, and in a DELETE's RETURNING too. These
# failed with 23514 before RP-655 was fixed.
OWN_DOMAIN = {
    "select pg_typeof(a) from t569nv order by a": (_REGTYPE, [("d569nv",), ("d569nv",)]),
    "select a::d569nv from t569nv order by a": (_INT4, [(-1,), (5,)]),
    "select array[a] from t569nv order by a": ("d569nv", ["{-1}", "{5}"]),
    "select array[a::d569nv] from t569nv order by a": ("d569nv", ["{-1}", "{5}"]),
    "select a::d569nv from t569nv where a::d569nv < 0": (_INT4, [(-1,)]),
    "select array(select a from t569nv order by a)": ("d569nv", ["{-1,5}"]),
    "select (select a from t569nv where a::d569nv < 0)": (_INT4, [(-1,)]),
    "delete from t569nv where a::d569nv < 0 returning a::d569nv": (_INT4, [(-1,)]),
}


@pytest.mark.backlog_fixed("RP-655")
@pytest.mark.parametrize("query", list(OWN_DOMAIN))
def test_cast_of_a_domain_column_to_its_domain_checks_nothing(query: str, dsn: str) -> None:
    ty, rows = OWN_DOMAIN[query]
    with _connect(dsn) as c:
        array = isinstance(ty, str)
        expected = (_typarray(c, ty), [(v,) for v in rows]) if array else (ty, rows)
        got = _answer(c, query)
    assert got == expected


_BPCHAR_ARRAY = 1014

# An ARRAY subquery is the array of its column's type, the column's
# modifier included: a `char(3)` value keeps its blanks in it, as it does
# in `ARRAY[c]` and `array_agg(c)`. The values were cast to `bpchar[]`, so
# they lost them, until RP-656 cast them to `character(3)[]`.
CHAR_ARRAYS = {
    "select array(select c from t569c)": (_BPCHAR_ARRAY, [(["xy "],)]),
    "with q as (select c from t569c) select array(select c from q)": (
        _BPCHAR_ARRAY,
        [(["xy "],)],
    ),
    "select array(select c from t569c union all select c from t569c)": (
        _BPCHAR_ARRAY,
        [(["xy ", "xy "],)],
    ),
}


@pytest.mark.backlog_fixed("RP-656")
@pytest.mark.parametrize("query", list(CHAR_ARRAYS))
def test_array_subquery_keeps_char_padding(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        got = _answer(c, query)
    assert got == CHAR_ARRAYS[query]


# The domain column keeps its domain however it is reached: through a join,
# a WITH or a set operation's branch, an array of it is the domain's array
# (whose text psycopg does not parse), with the domain's padding. These were
# `character[]`, the subqueries' without the padding, until RP-657.
REACHED_ARRAYS = {
    "select array[a] from t569d x join t569c on true order by i": ['{"a b"}', '{"xy "}'],
    "select array(select a from t569d x join t569c on true order by i)": ['{"a b","xy "}'],
    "with q as (select a, i from t569d) select array(select a from q order by i)": [
        '{"a b","xy "}'
    ],
    "select array(select a from t569d union all select a from t569d)": [
        '{"a b","xy ","a b","xy "}'
    ],
}


@pytest.mark.backlog_fixed("RP-657")
@pytest.mark.parametrize("query", list(REACHED_ARRAYS))
def test_domain_column_array_through_join_with_or_set_operation(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        expected = (_typarray(c, "d569c"), [(v,) for v in REACHED_ARRAYS[query]])
        got = _answer(c, query)
    assert got == expected
