"""Regression tests of RP-807 (a PL/pgSQL `FETCH ... INTO` a record typed a
domain column by its base type, so an assignment to the field skipped the
domain's CHECK), over every way a portal is made, and a gap test of a
divergence found while working it:

- a bound cursor's arguments (`c cursor (k int) for ...; open c(5)`) are
  not bound (RP-809).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t807 cascade",
    "drop domain if exists d807, dv807 cascade",
    "create domain d807 as int check (value > 0)",
    "create domain dv807 as varchar(3)",
    "create table t807 (a d807, v varchar(3), w dv807, c char(3))",
    "insert into t807 values (5, 'ab', 'q', 'x')",
]
_D807 = ("error", "23514", 'value for domain d807 violates check constraint "d807_check"')
_VARCHAR3 = ("error", "22001", "value too long for type character varying(3)")
_FETCH = (
    "do $$ declare r record; c cursor for select * from t807; "
    "begin open c; fetch c into r; %s end $$"
)
# A SQL DECLARE in the block, then a PL/pgSQL FETCH from it by name.
_SQL_FETCH = "do $$ declare r record; c refcursor := 's'; begin fetch c into r; %s end $$"


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:2]:
            c.execute(q)


def _answer(dsn: str, query: str, declare: str | None = None) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            if declare is None:
                return c.execute(query).statusmessage
            c.execute("begin")
            try:
                c.execute(declare)
                return c.execute(query).statusmessage
            finally:
                c.execute("rollback")
        except psycopg.Error as e:
            return ("error", e.sqlstate, e.diag.message_primary)


# The portal's row type keeps each column's domain, however it was opened.
OPEN_FETCH = {
    _FETCH % "raise exception '%', pg_typeof(r.w);": ("error", "P0001", "dv807"),
    _FETCH % "r.w := 'abcdef';": _VARCHAR3,
    _FETCH % "r.v := 'abcdef';": _VARCHAR3,
    _FETCH % "raise exception '%', pg_typeof(r.c);": ("error", "P0001", "character"),
    "do $$ declare r record; c cursor for select a, a + 1 as b from t807; "
    "begin open c; fetch c into r; raise exception '% %', pg_typeof(r.a), pg_typeof(r.b); "
    "end $$": ("error", "P0001", "d807 integer"),
    "do $$ declare r record; c cursor for select a from t807; begin open c; "
    "fetch c into r; fetch c into r; raise exception '%', pg_typeof(r.a); end $$": (
        "error",
        "P0001",
        "d807",
    ),
    "do $$ declare r record; c refcursor; begin open c for select a, w from t807; "
    "fetch c into r; raise exception '% %', pg_typeof(r.a), pg_typeof(r.w); end $$": (
        "error",
        "P0001",
        "d807 dv807",
    ),
    "do $$ declare r record; c refcursor; begin open c for execute 'select a from t807'; "
    "fetch c into r; r.a := -5; end $$": _D807,
}


@pytest.mark.backlog_fixed("RP-807")
@pytest.mark.parametrize("query", list(OPEN_FETCH))
def test_fetch_from_an_open_keeps_the_domains(query: str, dsn: str) -> None:
    """Was: a fetched record's field was of its column's base type (RP-807)."""
    assert _answer(dsn, query) == OPEN_FETCH[query]


SQL_DECLARE_FETCH = {
    (
        "declare s cursor for select a, w from t807",
        _SQL_FETCH % "raise exception '% %', pg_typeof(r.a), pg_typeof(r.w);",
    ): ("error", "P0001", "d807 dv807"),
    ("declare s cursor for select a from t807", _SQL_FETCH % "r.a := -5;"): _D807,
    (
        "declare s cursor for select * from t807 where a > 0 order by a",
        _SQL_FETCH % "r.a := -5;",
    ): _D807,
    (
        "declare s scroll cursor with hold for select a from t807",
        _SQL_FETCH % "r.a := -5;",
    ): _D807,
}


@pytest.mark.backlog_fixed("RP-807")
@pytest.mark.parametrize("case", list(SQL_DECLARE_FETCH))
def test_fetch_from_a_sql_declare_keeps_the_domains(case: tuple[str, str], dsn: str) -> None:
    """Was: a SQL DECLARE's cursor kept only its wire description, so a
    PL/pgSQL FETCH from it typed a domain column by its base (RP-807)."""
    declare, query = case
    assert _answer(dsn, query, declare) == SQL_DECLARE_FETCH[case]


BOUND_ARGS = {
    "do $$ declare r int; c cursor (k int) for select k; begin open c(5); "
    "fetch c into r; raise exception '%', r; end $$": ("error", "P0001", "5"),
    "do $$ declare r int; c cursor (k int) for select a from t807 where a = k; "
    "begin open c(5); fetch c into r; raise exception '%', r; end $$": ("error", "P0001", "5"),
    "do $$ declare r record; c cursor (k int) for select a from t807 where a = k; "
    "begin open c(5); fetch c into r; r.a := -5; end $$": _D807,
}


@pytest.mark.backlog("RP-809")
@pytest.mark.parametrize("query", list(BOUND_ARGS))
def test_bound_cursor_arguments_are_bound(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == BOUND_ARGS[query]
