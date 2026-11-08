"""Gap tests of what was found while working RP-527: RP-545 (a FROM item
naming no relation is reported after the statement's qualified column
references) and RP-546 (a function over a table's row type does not
depend on the table, fixed: its tests are regression tests).

An answer is the rows, `"ok"` for a statement with none, or `(sqlstate,
message, detail, position)` for an error. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _run(c: psycopg.Connection, query: str) -> Any:
    try:
        cur = c.execute(query)
    except psycopg.Error as e:
        return (
            e.sqlstate,
            e.diag.message_primary,
            e.diag.message_detail,
            e.diag.statement_position,
        )
    return cur.fetchall() if cur.description else "ok"


# `transformFromClause` runs before the select list and WHERE are read, so
# a FROM relation that does not exist is 42P01 `relation "nosuch" does not
# exist` at its name, whatever the qualified references say. Here the
# qualifier is checked first: `fs.c` is `missing FROM-clause entry`, and
# `nosuch.c` has the right message at the reference.
MISSING_FROM_RELATION = {
    "select fs.c from nosuch": ("42P01", 'relation "nosuch" does not exist', None, "18"),
    "select nosuch.c from nosuch": ("42P01", 'relation "nosuch" does not exist', None, "22"),
    "select a from nosuch where zz.a = 1": (
        "42P01",
        'relation "nosuch" does not exist',
        None,
        "15",
    ),
}


@pytest.mark.backlog("RP-545")
@pytest.mark.parametrize("query", list(MISSING_FROM_RELATION))
def test_missing_from_relation_reported_first(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists nosuch")
        assert _run(c, query) == MISSING_FROM_RELATION[query]


# A function whose argument is a table's row type depends on that type
# (`pg_depend`), so a plain DROP TABLE is 2BP01 and CASCADE drops the
# function too. The DROP used to succeed and leave the function over a
# type that no longer existed.
@pytest.mark.backlog_fixed("RP-546")
def test_row_type_function_depends_on_its_table(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists rq534 cascade")
        c.execute("drop function if exists rq534f(int)")
        c.execute("create table rq534 (a int, c text)")
        c.execute(
            "create or replace function rq534f(r rq534) returns text"
            " language sql as $$ select r.c $$"
        )
        try:
            got = [
                _run(c, "drop table rq534"),
                _run(c, "drop table rq534 cascade"),
                _run(c, "select count(*) from pg_proc where proname = 'rq534f'"),
            ]
        finally:
            c.execute("drop table if exists rq534 cascade")
    assert got == [
        (
            "2BP01",
            "cannot drop table rq534 because other objects depend on it",
            "function rq534f(rq534) depends on type rq534",
            None,
        ),
        "ok",
        [(0,)],
    ]


def _refused(name: str, *lines: str) -> tuple[str, str, str, None]:
    return (
        "2BP01",
        f"cannot drop table {name} because other objects depend on it",
        "\n".join(lines),
        None,
    )


# Every type a routine's signature names counts (`ProcedureCreate`): an
# argument of the row type's array, a SETOF result, an OUT parameter and a
# RETURNS TABLE column. What names the array comes first, then the rest in
# creation order, all ahead of the table's views and referencing foreign
# keys; a routine is `function name(input types)` joined by a bare comma,
# a procedure's too, its name quoted and qualified as `format_procedure`
# writes it; and CASCADE drops each, with a cast one of them implements.
@pytest.mark.backlog_fixed("RP-546")
def test_row_type_dependants_as_postgres_names_them(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists rq546b, rq546 cascade")
        for sql in [
            "create table rq546 (a int primary key, c text)",
            "create table rq546b (b int references rq546)",
            "create view rq546v as select * from rq546",
            "create function rq546f(x int, rq546) returns int language sql as $$ select 1 $$",
            "create function rq546g() returns setof rq546 language sql"
            " as $$ select * from rq546 $$",
            "create function rq546h(x int, out o rq546) language sql as $$ select null::rq546 $$",
            "create function rq546i(x int) returns table(o rq546, p int) language sql"
            " as $$ select null::rq546, 1 $$",
            'create procedure "Rq 546"(x rq546) language sql as $$ select 1 $$',
            "create function rq546a(r rq546[]) returns int language sql as $$ select 1 $$",
            "create function rq546c(rq546) returns int language sql as $$ select 1 $$",
            "create cast (rq546 as int) with function rq546c(rq546)",
        ]:
            c.execute(sql)
        notices: list[tuple[str, str | None]] = []
        c.add_notice_handler(lambda d: notices.append((d.message_primary, d.message_detail)))
        try:
            got = [
                _run(c, "drop table rq546"),
                _run(c, "drop table rq546 cascade"),
                _run(
                    c,
                    "select count(*) from pg_proc where proname like 'rq546%'"
                    " or proname = 'Rq 546'",
                ),
            ]
        finally:
            c.execute("drop table if exists rq546b, rq546 cascade")
    lines = [
        "function rq546a(rq546[]) depends on type rq546[]",
        "function rq546f(integer,rq546) depends on type rq546",
        "function rq546g() depends on type rq546",
        "function rq546h(integer) depends on type rq546",
        "function rq546i(integer) depends on type rq546",
        'function "Rq 546"(rq546) depends on type rq546',
        "function rq546c(rq546) depends on type rq546",
        "cast from rq546 to integer depends on type rq546",
        "constraint rq546b_b_fkey on table rq546b depends on table rq546",
        "view rq546v depends on table rq546",
    ]
    assert got[0] == _refused("rq546", *lines)
    assert got[1:3] == ["ok", [(0,)]]
    assert [n for n in notices if n[0].startswith("drop cascades")] == [
        (
            "drop cascades to 10 other objects",
            "\n".join("drop cascades to " + line.split(" depends on ")[0] for line in lines),
        )
    ]


# A routine in another schema over a schema's table, and one there: each
# qualified as `format_type_be` and `format_procedure` qualify what is off
# the search path.
@pytest.mark.backlog_fixed("RP-546")
def test_row_type_dependants_in_a_schema(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop schema if exists rq546s cascade")
        c.execute("create schema rq546s")
        c.execute("create table rq546s.t (a int)")
        c.execute("create function rq546s.h(rq546s.t) returns int language sql as $$ select 1 $$")
        c.execute("create function rq546p(rq546s.t) returns int language sql as $$ select 1 $$")
        try:
            got = [
                _run(c, "drop table rq546s.t"),
                _run(c, "drop table rq546s.t cascade"),
                _run(c, "select count(*) from pg_proc where proname in ('h', 'rq546p')"),
            ]
        finally:
            c.execute("drop schema if exists rq546s cascade")
            c.execute("drop function if exists rq546p")
    assert got == [
        _refused(
            "rq546s.t",
            "function rq546s.h(rq546s.t) depends on type rq546s.t",
            "function rq546p(rq546s.t) depends on type rq546s.t",
        ),
        "ok",
        [(0,)],
    ]


# A view's and a materialized view's row types are named the same way: DROP
# VIEW and DROP MATERIALIZED VIEW are refused under their own names, and
# nothing is removed by the refusal. A table turned into a view by CREATE
# RULE "_RETURN" keeps its row type, and what names it.
@pytest.mark.backlog_fixed("RP-546")
def test_view_row_type_dependants(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop view if exists rq546cv, rq546vv cascade")
        c.execute("drop materialized view if exists rq546mv cascade")
        c.execute("drop table if exists rq546cv cascade")
        try:
            got = [
                _run(c, sql)
                for sql in [
                    "create table rq546cv (a int)",
                    "create function rq546cvf(rq546cv) returns int language sql as $$ select 7 $$",
                    'create rule "_RETURN" as on select to rq546cv do instead select 1 as a',
                    "select relkind::text from pg_class where relname = 'rq546cv'",
                    "select rq546cvf(null::rq546cv)",
                    "drop view rq546cv",
                    "drop view rq546cv cascade",
                    "create materialized view rq546mv as select 1 as a",
                    "create function rq546mf(rq546mv) returns int language sql as $$ select 1 $$",
                    "drop materialized view rq546mv",
                    "select count(*) from pg_class where relname = 'rq546mv'",
                    "drop materialized view rq546mv cascade",
                    "create view rq546vv as select 1 as a",
                    "create function rq546vf(rq546vv) returns int language sql as $$ select 1 $$",
                    "drop view rq546vv",
                    "drop view rq546vv cascade",
                    "select count(*) from pg_proc"
                    " where proname in ('rq546cvf', 'rq546mf', 'rq546vf')",
                ]
            ]
        finally:
            c.execute("drop view if exists rq546cv, rq546vv cascade")
            c.execute("drop materialized view if exists rq546mv cascade")
            c.execute("drop table if exists rq546cv cascade")

    def refused(kind: str, name: str, line: str) -> tuple[str, str, str, None]:
        return (
            "2BP01",
            f"cannot drop {kind} {name} because other objects depend on it",
            line,
            None,
        )

    assert got == [
        "ok",
        "ok",
        "ok",
        [("v",)],
        [(7,)],
        refused("view", "rq546cv", "function rq546cvf(rq546cv) depends on type rq546cv"),
        "ok",
        "ok",
        "ok",
        refused(
            "materialized view",
            "rq546mv",
            "function rq546mf(rq546mv) depends on type rq546mv",
        ),
        [(1,)],
        "ok",
        "ok",
        "ok",
        refused("view", "rq546vv", "function rq546vf(rq546vv) depends on type rq546vv"),
        "ok",
        [(0,)],
    ]
