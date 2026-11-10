"""Gap tests of PL/pgSQL divergences found while working RP-809 (a bound
cursor's arguments were not bound) and RP-817:

- a FOR loop over a bound cursor was refused as a gap (fixed, RP-817);
- a variable declared `%TYPE` could not be assigned: its type was the text
  as written (fixed, RP-818);
- a cursor FOR loop's portal is not pinned: a CLOSE of it in the loop, and
  a COMMIT or ROLLBACK in a procedure's loop, drop it.

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
        c.execute("drop procedure if exists p817c(), p817r()")
        c.execute("drop procedure if exists p817c(), p817r()")


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
    # FOUND is set when the loop ends, by whether a row was read; the body
    # sees the FOUND it was entered with.
    "do $$ declare r text := ''; c cursor for select a from t809 where a > 5; "
    "begin perform 1; for x in c loop r := r || 'x'; end loop; "
    "raise exception '% %', found, r; end $$": ("error", "P0001", "f "),
    "do $$ declare r text := ''; c cursor for select a from t809; "
    "begin perform 1 where false; for x in c loop r := r || found::text; end loop; "
    "raise exception '% %', found, r; end $$": ("error", "P0001", "t falsefalse"),
    # ROW_COUNT is the last SQL statement's, not the loop's.
    "do $$ declare c cursor for select a from t809; n int; begin perform 1 from t809; "
    "for x in c loop end loop; get diagnostics n = row_count; "
    "raise exception '%', n; end $$": ("error", "P0001", "2"),
    # The portal is named by the variable, which reads its own name before
    # any OPEN, shows in pg_cursors while the loop runs, and is closed after.
    "do $$ declare c cursor for select 1; begin raise exception '%', c; end $$": (
        "error",
        "P0001",
        "c",
    ),
    "do $$ declare c cursor for select a from t809; n int; s text; begin "
    "for x in c loop select count(*), max(statement) into n, s from pg_cursors "
    "where name = 'c'; exit; end loop; raise exception '% %', n, s; end $$": (
        "error",
        "P0001",
        "1 select a from t809",
    ),
    "do $$ declare c cursor for select a from t809; n int; begin for x in c loop "
    "end loop; select count(*) into n from pg_cursors where name = 'c'; "
    "raise exception '% %', c, n; end $$": ("error", "P0001", "c 0"),
    "do $$ declare c cursor for select 1; begin c := 'zz'; for x in c loop "
    "raise exception '% %', c, (select string_agg(name, ',') from pg_cursors); "
    "end loop; end $$": ("error", "P0001", "zz zz"),
    # A NULL variable opens an unnamed portal, and is NULL again after.
    "do $$ declare c cursor for select a from t809 order by a; r text := ''; begin "
    "c := null; for x in c loop r := r || x.a; end loop; "
    "raise exception '% %', r, c is null; end $$": ("error", "P0001", "12 t"),
    # A FETCH in the body moves the loop's portal.
    "do $$ declare r text := ''; y int; c cursor for select a from t809 union all "
    "select 3 order by 1; begin for x in c loop r := r || x.a; fetch c into y; "
    "end loop; raise exception '%', r; end $$": ("error", "P0001", "13"),
    # A portal of the name already open is 42P03, before the arguments run,
    # at a FOR as at an OPEN.
    "do $$ declare c cursor for select a from t809; begin open c; "
    "for x in c loop end loop; end $$": ("error", "42P03", 'cursor "c" already in use'),
    "do $$ declare c cursor (k int) for select k; begin open c(1); "
    "for x in c(1/0) loop end loop; end $$": (
        "error",
        "42P03",
        'cursor "c" already in use',
    ),
    "do $$ declare c cursor (k int) for select k; begin open c(1); open c(1/0); end $$": (
        "error",
        "42P03",
        'cursor "c" already in use',
    ),
    "do $$ declare c cursor for select a from t809 order by a; begin "
    "for x in c loop for y in c loop end loop; end loop; end $$": (
        "error",
        "42P03",
        'cursor "c" already in use',
    ),
    # EXIT, CONTINUE and a caught error leave no portal behind.
    "do $$ declare r text := ''; c cursor for select a from t809 order by a; begin "
    "<<l>> for x in c loop continue l when x.a = 1; r := r || x.a; end loop; "
    "raise exception '%', r; end $$": ("error", "P0001", "2"),
    "do $$ declare c cursor for select a from t809; n int; begin <<l>> for x in c "
    "loop exit l; end loop; select count(*) into n from pg_cursors; "
    "raise exception '%', n; end $$": ("error", "P0001", "0"),
    "do $$ declare c cursor for select a from t809; n int; begin begin "
    "for x in c loop perform 1/0; end loop; exception when others then null; end; "
    "select count(*) into n from pg_cursors; raise exception '%', n; end $$": (
        "error",
        "P0001",
        "0",
    ),
    "do $$ declare r text := ''; c cursor for select a from t809 order by a; begin "
    "for x in c loop r := r || x.a; end loop; for x in c loop r := r || x.a; "
    "end loop; raise exception '%', r; end $$": ("error", "P0001", "1212"),
    # Named arguments, and the arguments' errors.
    "do $$ declare r int; c cursor (k int, j int) for select k - j as d; begin "
    "for x in c(j := 1, k := 5) loop r := x.d; end loop; "
    "raise exception '%', r; end $$": ("error", "P0001", "4"),
    "do $$ declare c cursor (k int) for select k; begin for x in c(1/0) loop end loop; end $$": (
        "error",
        "22012",
        "division by zero",
    ),
    "do $$ declare c cursor (k int) for select k; begin for x in c('a') loop end loop; end $$": (
        "error",
        "22P02",
        'invalid input syntax for type integer: "a"',
    ),
    "do $$ declare c cursor (k int) for select k; begin for x in c loop end loop; end $$": (
        "error",
        "42601",
        'cursor "c" has arguments',
    ),
    # The loop variable is a record of the portal's row type.
    "do $$ declare c cursor for select v from t809 order by a; begin "
    "for x in c loop raise exception '%', pg_typeof(x.v); end loop; end $$": (
        "error",
        "P0001",
        "character varying",
    ),
}


@pytest.mark.backlog_fixed("RP-817")
@pytest.mark.parametrize("query", list(FOR_OVER_CURSOR))
def test_for_over_a_bound_cursor(query: str, dsn: str) -> None:
    """A FOR loop over a bound cursor opens it as an OPEN does and walks the
    portal (`exec_stmt_forc`); it was refused as a gap (RP-817)."""
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


PINNED_PORTAL = {
    # `exec_for_query` pins the portal: a CLOSE of it is 24000, by PL/pgSQL
    # or SQL, and CLOSE ALL refuses it too.
    "do $$ declare c cursor for select a from t809; begin for x in c loop close c; "
    "end loop; end $$": ("error", "24000", 'cannot drop pinned portal "c"'),
    "do $$ declare c cursor for select a from t809; begin for x in c loop "
    "execute 'close c'; end loop; end $$": (
        "error",
        "24000",
        'cannot drop pinned portal "c"',
    ),
    "do $$ declare c cursor for select a from t809; begin for x in c loop "
    "execute 'close all'; end loop; end $$": (
        "error",
        "24000",
        'cannot drop pinned portal "c"',
    ),
    # A COMMIT or ROLLBACK in a procedure's loop holds the pinned portal
    # (`HoldPinnedPortals`), and the loop goes on.
    "call p817c()": ("error", "P0001", "12"),
    "call p817r()": ("error", "P0001", "12"),
}

_PROCEDURES = [
    "create or replace procedure p817c() language plpgsql as $$ declare c cursor for "
    "select a from t809 order by a; r text := ''; begin for x in c loop "
    "r := r || x.a; commit; end loop; raise exception '%', r; end $$",
    "create or replace procedure p817r() language plpgsql as $$ declare c cursor for "
    "select a from t809 order by a; r text := ''; begin for x in c loop "
    "r := r || x.a; rollback; end loop; raise exception '%', r; end $$",
]


@pytest.mark.backlog("RP-823")
@pytest.mark.parametrize("query", list(PINNED_PORTAL))
def test_a_cursor_loops_portal_is_pinned(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP + _PROCEDURES:
            c.execute(q)
    assert _answer(dsn, query) == PINNED_PORTAL[query]
