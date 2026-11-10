"""Gap tests of two PL/pgSQL divergences found while working RP-809 (a bound
cursor's arguments were not bound):

- a FOR loop over a bound cursor is refused as a gap;
- a variable declared `%TYPE` could not be assigned: its type was the text
  as written (fixed, RP-818).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t809 cascade",
    "create table t809 (a int, v varchar(3))",
    "insert into t809 values (1, 'ab'), (2, 'cd')",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _answer(dsn: str, query: str) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            return c.execute(query).statusmessage
        except psycopg.Error as e:
            return ("error", e.sqlstate, e.diag.message_primary)


FOR_OVER_CURSOR = {
    "do $$ declare t int := 0; c cursor (k int) for select a from t809 where a >= k; "
    "begin for x in c(1) loop t := t + x.a; end loop; raise exception '%', t; end $$": (
        "error",
        "P0001",
        "3",
    ),
    "do $$ declare t text := ''; c cursor for select v from t809 order by a; "
    "begin for x in c loop t := t || x.v; end loop; raise exception '%', t; end $$": (
        "error",
        "P0001",
        "abcd",
    ),
}


@pytest.mark.backlog("RP-817")
@pytest.mark.parametrize("query", list(FOR_OVER_CURSOR))
def test_for_over_a_bound_cursor(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == FOR_OVER_CURSOR[query]


PERCENT_TYPE = {
    "do $$ declare k t809.v%type := 'ab'; begin raise exception '% %', k, pg_typeof(k); end $$": (
        "error",
        "P0001",
        "ab character varying",
    ),
    "do $$ declare k t809.v%type; begin k := 'abcd'; end $$": (
        "error",
        "22001",
        "value too long for type character varying(3)",
    ),
    "do $$ declare a int; k a%type; begin k := 7; raise exception '% %', k, pg_typeof(k); end $$": (
        "error",
        "P0001",
        "7 integer",
    ),
    "do $$ declare a int; k a%type; begin k := 'x'; end $$": (
        "error",
        "22P02",
        'invalid input syntax for type integer: "x"',
    ),
}


@pytest.mark.backlog_fixed("RP-818")
@pytest.mark.parametrize("query", list(PERCENT_TYPE))
def test_a_percent_type_variable_is_assigned(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == PERCENT_TYPE[query]
