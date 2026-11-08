"""Gap tests of the divergences found while working RP-551 (a cast to a
domain over `char(n)` is blank-padded): RP-557 (a domain-typed result
column goes out with no type modifier), RP-558 (a domain column's
`atttypmod` is the domain's) and RP-559 (an array of a domain is described
and printed as `varchar[]`).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists fd551 cascade",
    "drop domain if exists d551c, d551n, d551t cascade",
    "create domain d551c as char(3)",
    "create domain d551n as numeric(5,2)",
    "create domain d551t as timestamp(0)",
    "create table fd551 (a d551c)",
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


# `SendRowDescriptionMessage` sends a domain column as its base type AND the
# base's modifier (`getBaseTypeAndTypmod`), so a cast to a domain over
# char(3) describes typmod 7, as `'ab'::char(3)` does.
WIRE_TYPMOD = {
    "select 'ab'::d551c": 7,
    "select 1.5::d551n": 327686,
    "select now()::timestamp::d551t": 0,
}


@pytest.mark.backlog("RP-557")
@pytest.mark.parametrize("query", list(WIRE_TYPMOD))
def test_domain_result_column_typmod(query: str, dsn: str) -> None:
    with _connect(dsn) as c:
        cur = c.execute(query)
        assert cur.pgresult is not None
        assert cur.pgresult.fmod(0) == WIRE_TYPMOD[query]


# A column declared with a domain carries no modifier of its own: its
# `atttypmod` is -1 (the domain's `typtypmod` holds the width).
@pytest.mark.backlog("RP-558")
def test_domain_column_atttypmod(dsn: str) -> None:
    with _connect(dsn) as c:
        got = c.execute(
            "select atttypmod from pg_attribute"
            " where attrelid = 'fd551'::regclass and attname = 'a'"
        ).fetchall()
    assert got == [(-1,)]


# An array of a domain is of the domain's array type (`typarray`), and its
# text quotes an element holding a blank, as `array_out` does.
@pytest.mark.backlog("RP-559")
def test_domain_array(dsn: str) -> None:
    with _connect(dsn) as c:
        (typarray,) = c.execute(
            "select typarray::int from pg_type where typname = 'd551c'"
        ).fetchone()
        cur = c.execute("select array['a b'::d551c]")
        assert cur.description is not None
        got = (cur.description[0].type_code, cur.fetchall())
    assert got == (typarray, [('{"a b"}',)])
