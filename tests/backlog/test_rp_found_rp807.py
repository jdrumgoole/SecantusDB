"""Regression tests of RP-807 (a PL/pgSQL `FETCH ... INTO` a record typed a
domain column by its base type, so an assignment to the field skipped the
domain's CHECK), over every way a portal is made, and of a divergence
found while working it:

- a bound cursor's arguments (`c cursor (k int) for ...; open c(5)`) were
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
    # Named notation, in any order, and mixed with positional.
    "do $$ declare r text; c cursor (k int, s text) for select s || k; "
    "begin open c(s := 'x', k := 3); fetch c into r; raise exception '%', r; end $$": (
        "error",
        "P0001",
        "x3",
    ),
    "do $$ declare r text; c cursor (k int, s text) for select s || k; "
    "begin open c(3, s := 'y'); fetch c into r; raise exception '%', r; end $$": (
        "error",
        "P0001",
        "y3",
    ),
    # Each value moves in by the assignment rule: rounded, typed, checked.
    "do $$ declare r text; c cursor (k int) for select k; "
    "begin open c(2.5); fetch c into r; raise exception '%', r; end $$": ("error", "P0001", "3"),
    "do $$ declare r text; c cursor (k int) for select pg_typeof(k); "
    "begin open c('7'); fetch c into r; raise exception '%', r; end $$": (
        "error",
        "P0001",
        "integer",
    ),
    "do $$ declare r text; c cursor (k varchar(2)) for select k; "
    "begin open c('abc'); fetch c into r; end $$": (
        "error",
        "22001",
        "value too long for type character varying(2)",
    ),
    "do $$ declare r text; c cursor (k int) for select k; "
    "begin open c('x'); fetch c into r; end $$": (
        "error",
        "22P02",
        'invalid input syntax for type integer: "x"',
    ),
    "do $$ declare r text; c cursor (k d807) for select k; "
    "begin open c(-1); fetch c into r; end $$": _D807,
    "do $$ declare r text; c cursor (k d807) for select pg_typeof(k); "
    "begin open c(4); fetch c into r; raise exception '%', r; end $$": ("error", "P0001", "d807"),
    # The arguments' level is labelled with the cursor's name, and is the
    # innermost: it hides a block variable of the same name.
    "do $$ declare r int; c cursor (k int) for select c.k; "
    "begin open c(9); fetch c into r; raise exception '%', r; end $$": ("error", "P0001", "9"),
    "do $$ declare k int := 1; r int; c cursor (k int) for select k; "
    "begin open c(6); fetch c into r; raise exception '%', r; end $$": ("error", "P0001", "6"),
    "do $$ declare r int; c cursor (lo int, hi int) for "
    "select count(*) from t807 where a between lo and hi; "
    "begin open c(1, 6); fetch c into r; raise exception '%', r; end $$": ("error", "P0001", "1"),
    "do $$ declare x int := 40; r int; c cursor (k int) for select k; "
    "begin open c(x + 2); fetch c into r; raise exception '%', r; end $$": ("error", "P0001", "42"),
    "do $$ declare r int; c cursor (k int) for select k; "
    "begin open c((select max(a) from t807)); fetch c into r; raise exception '%', r; end $$": (
        "error",
        "P0001",
        "5",
    ),
    "do $$ declare r int; s int; c cursor (k int) for select k; begin open c(1); "
    "fetch c into r; close c; open c(2); fetch c into s; raise exception '% %', r, s; end $$": (
        "error",
        "P0001",
        "1 2",
    ),
    # The arguments are a SELECT INTO: FOUND and ROW_COUNT are set by it,
    # and a second row is not read.
    "do $$ declare n int; c cursor (k int) for select a from t807 where a > k; "
    "begin perform 1 where false; open c(0); get diagnostics n = row_count; "
    "raise exception '% %', found, n; end $$": ("error", "P0001", "t 1"),
    "do $$ declare n int; r int; c cursor (k int) for select k; "
    "begin open c(generate_series(4, 6)); get diagnostics n = row_count; fetch c into r; "
    "raise exception '% % %', found, n, r; end $$": ("error", "P0001", "t 1 4"),
}


@pytest.mark.backlog_fixed("RP-809")
@pytest.mark.parametrize("query", list(BOUND_ARGS))
def test_bound_cursor_arguments_are_bound(query: str, dsn: str) -> None:
    """Was: the OPEN of a bound cursor read no arguments, so its query was
    planned with no `k` in scope (42703), or read a block variable of the
    same name (RP-809)."""
    assert _answer(dsn, query) == BOUND_ARGS[query]


_ARG_FRAME = "PL/pgSQL function inline_code_block line 1 at OPEN"
# An error running the argument list carries its `SQL expression` frame,
# but not a literal's input error, which has a position instead; an error
# moving a value into its variable has neither.
BOUND_ARG_CONTEXT = {
    "do $$ declare c cursor (k int) for select k; begin open c(1/0); end $$": (
        "22012",
        'SQL expression "1/0"\n' + _ARG_FRAME,
    ),
    "do $$ declare c cursor (k int) for select k; begin open c('x'::int); end $$": (
        "22P02",
        _ARG_FRAME,
    ),
    "do $$ declare c cursor (k int) for select k; begin open c('x'); end $$": (
        "22P02",
        _ARG_FRAME,
    ),
}


@pytest.mark.backlog_fixed("RP-809")
@pytest.mark.parametrize("query", list(BOUND_ARG_CONTEXT))
def test_bound_cursor_argument_errors_are_framed(query: str, dsn: str) -> None:
    """Was: no argument was evaluated, so none of these errors was raised
    (RP-809)."""
    with psycopg.connect(dsn, autocommit=True) as c, pytest.raises(psycopg.Error) as e:
        c.execute(query)
    assert (e.value.sqlstate, e.value.diag.context) == BOUND_ARG_CONTEXT[query]
