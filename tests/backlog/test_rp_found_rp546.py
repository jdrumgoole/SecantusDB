"""Gap tests of what was found while working RP-546 (a function over a
table's row type depends on the table): RP-560 .. RP-568, and RP-858,
found while working RP-564, and RP-893, found while working RP-561.

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
# dropped by DROP TABLE ... CASCADE. It was neither, and outlived the
# table in `pg_operator`.
@pytest.mark.backlog_fixed("RP-561")
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


# The operators interleave with the routines in creation order, what names
# the array type ahead of what names the row type, and a prefix operator's
# missing left argument is `NONE` (`format_operator`), in the DETAIL and in
# the CASCADE notice alike.
@pytest.mark.backlog_fixed("RP-561")
def test_operators_over_a_row_type_in_creation_order(dsn: str) -> None:
    notices: list[str | None] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda n: notices.append(n.message_detail))
        c.execute("drop table if exists q561 cascade")
        for q in [
            "create table q561 (a int)",
            "create function q561eq(q561, q561) returns bool language sql as $$ select true $$",
            "create operator ==== (leftarg = q561, rightarg = q561, function = q561eq)",
            "create function q561neg(q561) returns bool language sql as $$ select true $$",
            "create operator !!! (rightarg = q561, function = q561neg)",
            "create function q561arr(q561[], int) returns bool language sql as $$ select true $$",
            "create operator <<< (leftarg = q561[], rightarg = int, function = q561arr)",
        ]:
            c.execute(q)
        try:
            got = [
                _run(c, "drop table q561"),
                _run(c, "drop table q561 cascade"),
                _run(c, "select count(*) from pg_operator where oprname in ('====', '!!!', '<<<')"),
            ]
        finally:
            c.execute("drop table if exists q561 cascade")
    lines = [
        ("function q561arr(q561[],integer)", "type q561[]"),
        ("operator <<<(q561[],integer)", "type q561[]"),
        ("function q561eq(q561,q561)", "type q561"),
        ("operator ====(q561,q561)", "type q561"),
        ("function q561neg(q561)", "type q561"),
        ("operator !!!(NONE,q561)", "type q561"),
    ]
    assert got == [
        _refused("q561", *(f"{o} depends on {on}" for o, on in lines)),
        "ok",
        [(0,)],
    ]
    assert [n for n in notices if n and n.startswith("drop cascades")] == [
        "\n".join(f"drop cascades to {o}" for o, _ in lines)
    ]


# An operator depends on the function that implements it, so DROP FUNCTION
# of that function is refused while the operator exists, and CASCADE drops
# the operator. The function was dropped and the operator left in
# `pg_operator` naming it.
@pytest.mark.backlog_fixed("RP-893")
def test_drop_function_of_an_operator_is_refused(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists f561 cascade")
        c.execute("create table f561 (a int)")
        c.execute(
            "create function f561eq(f561, int) returns bool language sql as $$ select true $$"
        )
        c.execute("create operator |=| (leftarg = f561, rightarg = int, function = f561eq)")
        try:
            got = [
                _run(c, "drop function f561eq(f561, int)"),
                _run(c, "drop function f561eq(f561, int) cascade"),
                _run(c, "select count(*) from pg_operator where oprname = '|=|'"),
            ]
        finally:
            c.execute("drop table if exists f561 cascade")
    assert got == [
        (
            "2BP01",
            "cannot drop function f561eq(f561,integer) because other objects depend on it",
            "operator |=|(f561,integer) depends on function f561eq(f561,integer)",
        ),
        "ok",
        [(0,)],
    ]


# Two prefix operators over one function: RESTRICT names each, in creation
# order and with `NONE` for the missing left operand, and CASCADE counts
# them in its notice (RP-893).
@pytest.mark.backlog_fixed("RP-893")
def test_drop_function_of_two_prefix_operators(dsn: str) -> None:
    notices: list[tuple[str, str | None]] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda n: notices.append((n.message_primary, n.message_detail)))
        c.execute("drop function if exists p893neg(int) cascade")
        c.execute("create function p893neg(int) returns int language sql as $$ select -$1 $$")
        c.execute("create operator !!# (rightarg = int, function = p893neg)")
        c.execute("create operator ~~# (rightarg = int, function = p893neg)")
        try:
            got = [
                _run(c, "drop function p893neg(int)"),
                _run(c, "drop function p893neg(int) cascade"),
                _run(c, "select count(*) from pg_operator where oprname in ('!!#', '~~#')"),
            ]
        finally:
            c.execute("drop function if exists p893neg(int) cascade")
    assert got == [
        (
            "2BP01",
            "cannot drop function p893neg(integer) because other objects depend on it",
            "operator !!#(NONE,integer) depends on function p893neg(integer)\n"
            "operator ~~#(NONE,integer) depends on function p893neg(integer)",
        ),
        "ok",
        [(0,)],
    ]
    assert [n for n in notices if n[0].startswith("drop cascades")] == [
        (
            "drop cascades to 2 other objects",
            "drop cascades to operator !!#(NONE,integer)\n"
            "drop cascades to operator ~~#(NONE,integer)",
        )
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
# types joined by a bare comma. DROP TYPE's DETAIL here put a blank after
# each comma, the lookup errors' `func_signature_string` form.
@pytest.mark.backlog_fixed("RP-563")
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
# names several, says `desired object(s)`. A two-table DROP TABLE was
# refused under the first table's name.
@pytest.mark.backlog_fixed("RP-564")
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


# The several-table refusal's DETAIL reads `targetObjects` backwards: the
# last table named's dependants first, and the routine over both under the
# table named first. An IF EXISTS list with one table present keeps that
# table's name.
@pytest.mark.backlog_fixed("RP-564")
def test_multi_table_drop_detail_order(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists n564a, n564b cascade")
        c.execute("create table n564a (a int)")
        c.execute("create table n564b (a int)")
        c.execute("create function k564n(n564b, n564a) returns int language sql as $$ select 1 $$")
        c.execute("create view n564va as select * from n564a")
        c.execute("create view n564vb as select * from n564b")
        c.execute("create function kb564(n564b) returns int language sql as $$ select 1 $$")
        c.execute("create function ka564(n564a) returns int language sql as $$ select 1 $$")
        try:
            got = [
                _run(c, "drop table n564a, n564b"),
                _run(c, "drop table n564b, n564a"),
                _run(c, "drop table if exists nosuch564, n564a"),
            ]
        finally:
            c.execute("drop table if exists n564a, n564b cascade")
    many = "cannot drop desired object(s) because other objects depend on them"
    assert got == [
        (
            "2BP01",
            many,
            "function kb564(n564b) depends on type n564b\n"
            "view n564vb depends on table n564b\n"
            "function k564n(n564b,n564a) depends on type n564a\n"
            "function ka564(n564a) depends on type n564a\n"
            "view n564va depends on table n564a",
        ),
        (
            "2BP01",
            many,
            "function ka564(n564a) depends on type n564a\n"
            "view n564va depends on table n564a\n"
            "function k564n(n564b,n564a) depends on type n564b\n"
            "function kb564(n564b) depends on type n564b\n"
            "view n564vb depends on table n564b",
        ),
        _refused(
            "n564a",
            "function k564n(n564b,n564a) depends on type n564a",
            "function ka564(n564a) depends on type n564a",
            "view n564va depends on table n564a",
        ),
    ]


# A CASCADE DROP TABLE of several tables is one `performMultipleDeletions`
# and so one notice, the last table's dependants first. Here each table is
# dropped on its own, each with its own notice.
@pytest.mark.backlog("RP-858")
def test_multi_table_drop_cascade_notice(dsn: str) -> None:
    notices: list[tuple[str | None, str | None]] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists p858a, p858b cascade")
        c.execute("create table p858a (a int)")
        c.execute("create table p858b (a int)")
        c.execute("create function k858(p858b, p858a) returns int language sql as $$ select 1 $$")
        c.execute("create view p858va as select * from p858a")
        c.execute("create view p858vb as select * from p858b")
        c.add_notice_handler(
            lambda d: notices.append((d.message_primary, d.message_detail)),
        )
        try:
            got = (_run(c, "drop table p858a, p858b cascade"), list(notices))
        finally:
            c.execute("drop table if exists p858a, p858b cascade")
    assert got == (
        "ok",
        [
            (
                "drop cascades to 3 other objects",
                "drop cascades to view p858vb\n"
                "drop cascades to function k858(p858b,p858a)\n"
                "drop cascades to view p858va",
            )
        ],
    )


# A routine over a temporary table's row type names that type: its
# `proargtypes` is `{tt565}` and the table cannot be dropped without it.
# The argument used to resolve to no type (`-`), the DROP succeeded, and
# the routine was left naming nothing: a bare type name never searched the
# session's temporary schema.
@pytest.mark.backlog_fixed("RP-565")
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
# `interpret_function_parameter_list`). Both used to be created.
@pytest.mark.backlog_fixed("RP-566")
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


def _located(c: psycopg.Connection, query: str) -> Any:
    try:
        c.execute(query)
    except psycopg.Error as e:
        return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
    return "ok"


# What else `CreateFunction` looks up by name, measured on PostgreSQL 15.19:
# a parameter's type unquoted and with `[]` (`TypeNameToString`), a missing
# schema 3F000, then the result's type QUOTED (`compute_return_type`, which
# makes a shell only for a C or internal function), none with a position;
# a body's own lookup keeps its position. A relation's row type, a
# catalog's, a temp table's, a view's, a domain, an enum, a composite, a
# pseudo-type and `%TYPE` are all types a routine may name.
@pytest.mark.backlog_fixed("RP-566")
def test_routine_signature_types_are_looked_up(dsn: str) -> None:
    sql = "language sql as $$ select 1 $$"
    refused = [
        f"create function q566a(x nosuch566[]) returns int {sql}",
        f"create function q566b(x public.nosuch566) returns int {sql}",
        f"create function q566c(x nosch566.t) returns int {sql}",
        f"create function q566d(x int = 1, y nosuch566) returns int {sql}",
        f"create function q566e(x nosuch566, out y int) returns text {sql}",
        f"create function q566f() returns table (x nosuch566) {sql}",
        f"create function q566g(x text) returns nosuch566 {sql}",
        "create function q566h() returns setof nosuch566 language plpgsql as $$ begin end $$",
        f"create function q566i() returns public.nosuch566[] {sql}",
        f"create function q566j(x nosuch566) returns nosuch2 {sql}",
        "create function q566k() returns int language sql as $$ select 'x'::nosuch566 $$",
    ]
    accepted = [
        f"create function a566a(x t566, y t566[], z v566) returns int {sql}",
        "create function a566b(x pg_class, y pg_class[]) returns t566"
        " language sql as $$ select null::t566 $$",
        f"create function a566c(x tt566, y pg_temp.tt566) returns int {sql}",
        f"create function a566d(x d566, y c566, z e566) returns int {sql}",
        f"create function a566e(x anyelement, y t566.a%type) returns int {sql}",
        "create function a566f(x information_schema.tables) returns record"
        " language sql as $$ select 1, 2 $$",
        f"create procedure a566g(inout x d566) {sql}",
        "create function a566h() returns trigger language plpgsql as $$ begin return null; end $$",
    ]
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists t566 cascade")
        c.execute("drop domain if exists d566 cascade")
        c.execute("drop type if exists c566, e566 cascade")
        c.execute("create table t566 (a int)")
        c.execute("create view v566 as select a from t566")
        c.execute("create temp table tt566 (a int)")
        c.execute("create domain d566 as int")
        c.execute("create type c566 as (x int)")
        c.execute("create type e566 as enum ('x')")
        try:
            got = [_located(c, q) for q in refused + accepted]
        finally:
            for f in ("a566a", "a566b", "a566c", "a566d", "a566e", "a566f", "a566h"):
                c.execute(f"drop function if exists {f}")
            c.execute("drop procedure if exists a566g")
            c.execute("drop table if exists t566, tt566 cascade")
            c.execute("drop domain if exists d566 cascade")
            c.execute("drop type if exists c566, e566 cascade")
    missing = ("42704", "type nosuch566 does not exist", None)
    result = ("42704", 'type "nosuch566" does not exist', None)
    assert got == [
        ("42704", "type nosuch566[] does not exist", None),
        ("42704", "type public.nosuch566 does not exist", None),
        ("3F000", 'schema "nosch566" does not exist', None),
        missing,
        missing,
        missing,
        result,
        result,
        ("42704", 'type "public.nosuch566[]" does not exist', None),
        missing,
        ("42704", 'type "nosuch566" does not exist', "68"),
    ] + ["ok"] * len(accepted)


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
# w568v` after the view, and CASCADE drops the function. The DETAIL named
# only the view and the function outlived it, until RP-568.
@pytest.mark.backlog_fixed("RP-568")
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


# What names a view's row type is named after the view, ahead of the
# view's own readers, by DROP TABLE and by DROP VIEW and DROP MATERIALIZED
# VIEW of a relation under it; an object two row types reach is named once,
# under the newer relation (`reportDependentObjects` reads
# `findDependentObjects`' newest-first record backwards), and CASCADE drops
# it. Measured on a private PostgreSQL 15.19 cluster.
@pytest.mark.backlog_fixed("RP-568")
def test_row_type_dependants_through_nested_and_shared_views(dsn: str) -> None:
    sql = "language sql as $$ select 1 $$"
    with psycopg.connect(dsn, autocommit=True) as c:
        for t in ("x568", "y568", "z568"):
            c.execute(f"drop table if exists {t} cascade")
        c.execute("create table x568 (a int)")
        c.execute("create view x568v as select * from x568")
        c.execute("create view x568w as select * from x568v")
        c.execute(f"create function x568f(x568w) returns int {sql}")
        c.execute(f"create function x568g(x568v) returns int {sql}")
        c.execute("create table y568 (a int)")
        c.execute("create view y568v as select * from y568")
        c.execute("create view y568w as select * from y568")
        c.execute(f"create function y568f(y568v, y568w) returns int {sql}")
        c.execute("create function y568t(y568) returns y568v language sql as $$ select $1.a $$")
        c.execute("create table z568 (a int)")
        c.execute("create materialized view z568m as select * from z568")
        c.execute(f"create function z568f(z568m[]) returns int {sql}")
        c.execute("create view z568v as select * from z568m")
        c.execute(f"create function z568g(z568v) returns int {sql}")
        try:
            got = [
                _run(c, "drop table x568"),
                _run(c, "drop view x568v"),
                _run(c, "drop view x568v cascade"),
                _run(c, "drop table y568"),
                _run(c, "drop table y568 cascade"),
                _run(c, "drop table z568"),
                _run(c, "drop materialized view z568m"),
                _run(c, "drop table z568 cascade"),
                _run(
                    c,
                    "select proname from pg_proc where proname in "
                    "('x568f', 'x568g', 'y568f', 'y568t', 'z568f', 'z568g')",
                ),
            ]
        finally:
            for t in ("x568", "y568", "z568"):
                c.execute(f"drop table if exists {t} cascade")
    message = "cannot drop {} because other objects depend on it"
    assert got == [
        _refused(
            "x568",
            "view x568v depends on table x568",
            "function x568g(x568v) depends on type x568v",
            "view x568w depends on view x568v",
            "function x568f(x568w) depends on type x568w",
        ),
        (
            "2BP01",
            message.format("view x568v"),
            "function x568g(x568v) depends on type x568v\n"
            "view x568w depends on view x568v\n"
            "function x568f(x568w) depends on type x568w",
        ),
        "ok",
        _refused(
            "y568",
            "view y568v depends on table y568",
            "function y568t(y568) depends on type y568v",
            "view y568w depends on table y568",
            "function y568f(y568v,y568w) depends on type y568w",
        ),
        "ok",
        _refused(
            "z568",
            "materialized view z568m depends on table z568",
            "function z568f(z568m[]) depends on type z568m[]",
            "view z568v depends on materialized view z568m",
            "function z568g(z568v) depends on type z568v",
        ),
        (
            "2BP01",
            message.format("materialized view z568m"),
            "function z568f(z568m[]) depends on type z568m[]\n"
            "view z568v depends on materialized view z568m\n"
            "function z568g(z568v) depends on type z568v",
        ),
        "ok",
        [],
    ]
