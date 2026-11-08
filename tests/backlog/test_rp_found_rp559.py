"""Gap tests of the divergences found while working RP-559 (an array of a
domain is of the domain's array type): an array built from a domain-typed
COLUMN is typed by the column's base type, and a cast to a domain's array
does not apply the base type's modifier to its elements.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists fd559 cascade",
    "drop domain if exists d559c, d559i cascade",
    "create domain d559c as char(3)",
    "create domain d559i as int",
    "create table fd559 (a d559c, i d559i)",
    "insert into fd559 values ('a b', 7), ('xy', null)",
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


# A column declared with a domain is OF the domain (`exprType` of its Var),
# so an array built from it is the domain's array, whose text psycopg does
# not parse. The element keeps its blank padding.
COLUMN_ARRAYS = {
    "select array[a] from fd559 order by i": ("d559c", ['{"a b"}', '{"xy "}']),
    "select array[i] from fd559 order by i": ("d559i", ["{7}", "{NULL}"]),
    "select array_agg(a order by i) from fd559": ("d559c", ['{"a b","xy "}']),
    "select array(select a from fd559 order by i)": ("d559c", ['{"a b","xy "}']),
}


@pytest.mark.backlog("RP-569")
@pytest.mark.parametrize("query", list(COLUMN_ARRAYS))
def test_array_of_domain_column(query: str, dsn: str) -> None:
    domain, values = COLUMN_ARRAYS[query]
    with _connect(dsn) as c:
        expected = (_typarray(c, domain), [(v,) for v in values])
        got = _answer(c, query)
    assert got == expected


# A cast to a domain's array coerces each element to the domain, through
# the base type's modifier: `char(3)` pads a short element and refuses a
# long one.
CASTS = {
    "select '{ab}'::d559c[]": ("d559c", ['{"ab "}']),
    "select array['ab']::d559c[]": ("d559c", ['{"ab "}']),
    "select '{abcd}'::d559c[]": (
        None,
        ("error", "22001", "value too long for type character(3)"),
    ),
}


@pytest.mark.backlog("RP-570")
@pytest.mark.parametrize("query", list(CASTS))
def test_cast_to_domain_array_applies_typmod(query: str, dsn: str) -> None:
    domain, expected = CASTS[query]
    with _connect(dsn) as c:
        if domain is not None:
            assert isinstance(expected, list)
            expected = (_typarray(c, domain), [(v,) for v in expected])
        got = _answer(c, query)
    assert got == expected
