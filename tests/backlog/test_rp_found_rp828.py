"""Gap tests of divergences found while working RP-828 (a FOR loop over a
query or an EXECUTE ran no portal):

- a FOR loop or an OPEN over an INSERT, UPDATE or DELETE is 0A000 `this
  query is not supported on one side of a set operation`, with RETURNING
  or without;
- a FOR loop or an OPEN over `EXECUTE ... USING` does not bind the USING
  values: `$1` is 42P02;
- a portal whose query fails as it runs takes no `<unnamed portal N>`
  number, so the next portal's name is one lower than PostgreSQL's.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, each statement on a new connection.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t828 cascade",
    "drop procedure if exists p828",
    "create table t828 (a int)",
    "insert into t828 values (1)",
    "create procedure p828() language plpgsql as $$ declare x record; begin "
    "for x in insert into t828 values (9) returning a loop commit; end loop; end $$",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:2]:
            c.execute(q)


def _answer(dsn: str, statement: str, setup: bool = False) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP if setup else []:
            c.execute(q)
        notices: list[str] = []
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        try:
            c.execute(statement)
        except psycopg.Error as e:
            return ("error", e.sqlstate, e.diag.message_primary)
        return ("notice", notices)


DML_PORTAL = {
    # `SPI_cursor_open` takes any statement that returns rows: a RETURNING
    # makes a PORTAL_ONE_RETURNING portal of the DML.
    "do $$ declare x record; begin for x in update t828 set a = a + 1 returning a loop "
    "raise notice '%', x.a; end loop; end $$": ("notice", ["2"]),
    "do $$ declare x record; begin for x in execute 'update t828 set a = a + 1 returning a' "
    "loop raise notice '%', x.a; end loop; end $$": ("notice", ["2"]),
    "do $$ declare c refcursor; x record; begin open c for insert into t828 values (5) "
    "returning a; fetch c into x; raise notice '%', x.a; close c; end $$": ("notice", ["5"]),
    # `HoldPinnedPortals` holds only a read-only (PORTAL_ONE_SELECT) portal.
    "call p828()": (
        "error",
        "55000",
        "cannot perform transaction commands inside a cursor loop that is not read-only",
    ),
    # Without RETURNING the statement has no result (`SPI_cursor_open_internal`).
    "do $$ declare x record; begin for x in update t828 set a = a where false loop "
    "end loop; end $$": ("error", "42P11", "cannot open UPDATE query as cursor"),
}


@pytest.mark.backlog_fixed("RP-905")
@pytest.mark.parametrize("statement", list(DML_PORTAL))
def test_a_dml_statement_opens_as_a_portal(statement: str, dsn: str) -> None:
    """A FOR loop or an OPEN over an INSERT, UPDATE or DELETE was 0A000: the
    portal read rows only from a SELECT."""
    assert _answer(dsn, statement, setup=True) == DML_PORTAL[statement]


USING = {
    # `exec_dynquery_with_params` passes the USING values as `$n`.
    "do $$ declare x record; begin for x in execute 'select $1 as v, $2 as w' using 5, "
    "'q'::text loop raise notice '% %', x.v, x.w; end loop; end $$": ("notice", ["5 q"]),
    "do $$ declare c refcursor; x record; begin open c for execute 'select $1 as v' "
    "using 7; fetch c into x; raise notice '%', x.v; close c; end $$": ("notice", ["7"]),
}


@pytest.mark.backlog_fixed("RP-906")
@pytest.mark.parametrize("statement", list(USING))
def test_a_dynamic_portal_binds_its_using_values(statement: str, dsn: str) -> None:
    """A FOR loop or an OPEN over `EXECUTE ... USING` ran its query with no
    parameters, so `$1` was 42P02."""
    assert _answer(dsn, statement) == USING[statement]


NUMBERING = {
    # `SPI_cursor_open_internal` names the portal (`CreateNewPortal`)
    # before it runs the query, so one that fails running has taken
    # `<unnamed portal 1>`. (A constant `1/0` fails in the planner, first.)
    "do $$ declare x record; begin begin for x in select a/0 from (values (1)) v(a) loop "
    "end loop; exception when others then null; end; for x in select 1 loop "
    "raise notice '%', (select name from pg_cursors); end loop; end $$": (
        "notice",
        ["<unnamed portal 2>"],
    ),
    "do $$ declare c refcursor; x record; begin begin open c for select a/0 "
    "from (values (1)) v(a); exception when others then null; end; for x in select 1 loop "
    "raise notice '%', (select name from pg_cursors); end loop; end $$": (
        "notice",
        ["<unnamed portal 2>"],
    ),
}


@pytest.mark.backlog("RP-907")
@pytest.mark.parametrize("statement", list(NUMBERING))
def test_a_portal_that_fails_running_takes_its_number(statement: str, dsn: str) -> None:
    assert _answer(dsn, statement) == NUMBERING[statement]
