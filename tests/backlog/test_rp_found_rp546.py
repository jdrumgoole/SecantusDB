"""Gap tests of what was found while working RP-546 (a function over a
table's row type depends on the table): RP-560 .. RP-568.

An answer is the rows, `"ok"` for a statement with none, or `(sqlstate,
message, detail)` for an error. The expected answers were measured on a
private, freshly initdb'd PostgreSQL 15.19 cluster.
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
        return (e.sqlstate, e.diag.message_primary, e.diag.message_detail)
    return cur.fetchall() if cur.description else "ok"


def _refused(name: str, *lines: str) -> tuple[str, str, str]:
    return (
        "2BP01",
        f"cannot drop table {name} because other objects depend on it",
        "\n".join(lines),
    )


# `findDependentObjects` reads `pg_depend` in its index order, which for
# one referenced object is creation order: a view made before the table
# whose foreign key references it is named first, and a cast made between
# two functions over the row type falls between them. Here a table's
# foreign keys always come before its views, and the casts after every
# routine.
@pytest.mark.backlog("RP-560")
def test_drop_table_dependants_in_creation_order(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists o560b, o560a cascade")
        c.execute("create table o560a (a int primary key)")
        c.execute("create view o560v as select * from o560a")
        c.execute("create table o560b (b int references o560a)")
        try:
            views_first = _run(c, "drop table o560a")
            c.execute("drop table o560b")
            c.execute("drop view o560v")
            c.execute("create function o560i(o560a) returns int language sql as $$ select 1 $$")
            c.execute("create cast (o560a as int) with function o560i(o560a)")
            c.execute("create function o560j(o560a) returns text language sql as $$ select 'x' $$")
            cast_between = _run(c, "drop table o560a")
        finally:
            c.execute("drop table if exists o560b, o560a cascade")
    assert [views_first, cast_between] == [
        _refused(
            "o560a",
            "view o560v depends on table o560a",
            "constraint o560b_b_fkey on table o560b depends on table o560a",
        ),
        _refused(
            "o560a",
            "function o560i(o560a) depends on type o560a",
            "cast from o560a to integer depends on type o560a",
            "function o560j(o560a) depends on type o560a",
        ),
    ]


# `CREATE OPERATOR` records a dependency on its argument types, so an
# operator over a table's row type is named by a plain DROP TABLE and
# dropped by DROP TABLE ... CASCADE. Here it is neither, and outlives the
# table in `pg_operator`.
@pytest.mark.backlog("RP-561")
def test_operator_over_row_type_depends_on_its_table(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists p561 cascade")
        c.execute("create table p561 (a int)")
        c.execute(
            "create function p561eq(p561, p561) returns bool language sql as $$ select true $$"
        )
        c.execute("create operator ==== (leftarg = p561, rightarg = p561, function = p561eq)")
        try:
            got = [
                _run(c, "drop table p561"),
                _run(c, "drop table p561 cascade"),
                _run(c, "select count(*) from pg_operator where oprname = '===='"),
            ]
        finally:
            c.execute("drop table if exists p561 cascade")
    assert got == [
        _refused(
            "p561",
            "function p561eq(p561,p561) depends on type p561",
            "operator ====(p561,p561) depends on type p561",
        ),
        "ok",
        [(0,)],
    ]


# A table's row type is the table's (an internal dependency): DROP TYPE of
# it is `cannot drop type r562 because table r562 requires it`, whatever
# else names the type. Here a routine over it makes the answer the
# routines' 2BP01 instead (with no routine it already matches).
@pytest.mark.backlog("RP-562")
def test_drop_type_of_a_row_type_with_a_routine(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists r562 cascade")
        c.execute("create table r562 (a int)")
        c.execute("create function r562f(r562) returns int language sql as $$ select 1 $$")
        try:
            got = _run(c, "drop type r562")
        finally:
            c.execute("drop table if exists r562 cascade")
    assert got == ("2BP01", "cannot drop type r562 because table r562 requires it", None)


# `getObjectDescription` names a routine by `format_procedure`, its argument
# types joined by a bare comma. DROP TYPE's DETAIL here puts a blank after
# each comma.
@pytest.mark.backlog("RP-563")
def test_drop_type_names_a_routine_as_format_procedure(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop type if exists ty563 cascade")
        c.execute("create type ty563 as (a int)")
        c.execute("create function k563(int, ty563) returns int language sql as $$ select 1 $$")
        try:
            got = _run(c, "drop type ty563")
        finally:
            c.execute("drop type if exists ty563 cascade")
    assert got == (
        "2BP01",
        "cannot drop type ty563 because other objects depend on it",
        "function k563(integer,ty563) depends on type ty563",
    )


# `reportDependentObjects` names the one object dropped, or, when the DROP
# names several, says `desired object(s)`. Here a two-table DROP TABLE is
# refused under the first table's name.
@pytest.mark.backlog("RP-564")
def test_multi_table_drop_refusal_headline(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists m564a, m564b cascade")
        c.execute("create table m564a (a int)")
        c.execute("create table m564b (a int)")
        c.execute("create function k564(m564b, m564a) returns int language sql as $$ select 1 $$")
        try:
            got = _run(c, "drop table m564a, m564b")
        finally:
            c.execute("drop table if exists m564a, m564b cascade")
    assert got == (
        "2BP01",
        "cannot drop desired object(s) because other objects depend on them",
        "function k564(m564b,m564a) depends on type m564a",
    )


# A routine over a temporary table's row type names that type: its
# `proargtypes` is `{tt565}` and the table cannot be dropped without it.
# Here the argument resolves to no type (`-`), the DROP succeeds, and the
# routine is left naming nothing.
@pytest.mark.backlog("RP-565")
def test_routine_over_a_temp_table_row_type(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("create temp table tt565 (a int)")
        c.execute("create function tf565(tt565) returns int language sql as $$ select 1 $$")
        try:
            got = [
                _run(
                    c,
                    "select proargtypes::regtype[]::text from pg_proc where proname = 'tf565'",
                ),
                _run(c, "drop table tt565"),
            ]
        finally:
            c.execute("drop table if exists tt565 cascade")
            c.execute("drop function if exists tf565")
    assert got == [
        [("[0:0]={tt565}",)],
        _refused("tt565", "function tf565(tt565) depends on type tt565"),
    ]


# An argument type that does not exist is 42704 at CREATE FUNCTION and
# CREATE PROCEDURE in a SQL routine (`LookupTypeName` in
# `interpret_function_parameter_list`). Here both are created.
@pytest.mark.backlog("RP-566")
def test_routine_over_an_undefined_type_is_refused(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            got = [
                _run(c, "create procedure upp566(x nosuch566) language sql as $$ select 1 $$"),
                _run(
                    c,
                    "create function ufn566(x nosuch566) returns int language sql"
                    " as $$ select 1 $$",
                ),
            ]
        finally:
            c.execute("drop procedure if exists upp566")
            c.execute("drop function if exists ufn566")
    assert got == [
        ("42704", "type nosuch566 does not exist", None),
        ("42704", "type nosuch566 does not exist", None),
    ]


# `regprocedureout` is `format_procedure`: the name quoted as an identifier
# needs, and each argument's `format_type_be`, which qualifies a type off
# the search path without quoting the dot. Here the name goes unquoted and
# a schema's type is quoted whole (`"s567.t"`).
@pytest.mark.backlog("RP-567")
def test_regprocedure_quotes_as_format_procedure(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop schema if exists s567 cascade")
        c.execute("create schema s567")
        c.execute("create table s567.t (a int)")
        try:
            c.execute("create function s567.h(s567.t) returns int language sql as $$ select 1 $$")
            c.execute('create function "Q f567"(s567.t) returns int language sql as $$ select 1 $$')
            got = _run(
                c,
                "select oid::regprocedure::text from pg_proc"
                " where proname in ('h', 'Q f567') order by 1",
            )
        finally:
            c.execute("drop schema if exists s567 cascade")
            c.execute('drop function if exists "Q f567"')
    assert got == [('"Q f567"(s567.t)',), ("s567.h(s567.t)",)]


# A view dropped by DROP TABLE ... CASCADE takes what names its row type
# with it: `drop table w568` names `function w568f(w568v) depends on type
# w568v` after the view, and CASCADE drops the function. Here the DETAIL
# names only the view and the function outlives it. A direct DROP VIEW or
# DROP MATERIALIZED VIEW already answers as PostgreSQL does (RP-546).
@pytest.mark.backlog("RP-568")
def test_cascaded_view_takes_its_row_type_dependants(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists w568 cascade")
        c.execute("create table w568 (a int)")
        c.execute("create view w568v as select * from w568")
        c.execute("create function w568f(w568v) returns int language sql as $$ select 1 $$")
        try:
            got = [
                _run(c, "drop table w568"),
                _run(c, "drop table w568 cascade"),
                _run(c, "select count(*) from pg_proc where proname = 'w568f'"),
            ]
        finally:
            c.execute("drop table if exists w568 cascade")
            c.execute("drop function if exists w568f")
    assert got == [
        _refused(
            "w568",
            "view w568v depends on table w568",
            "function w568f(w568v) depends on type w568v",
        ),
        "ok",
        [(0,)],
    ]
