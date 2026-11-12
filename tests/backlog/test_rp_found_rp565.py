"""Gap tests of what was found while working RP-565 (a routine over a
temporary table's row type names no type): RP-889 .. RP-891.

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


# `TypenameGetTypid` walks `activeSearchPath`: the session's temporary
# schema first, then each schema on the path. A table's row type in a
# schema the path names was not found by its bare name (42704 for a cast,
# `-` in `proargtypes` and no dependency for a routine), nor were the
# `_t` and `pg_temp.t` spellings; and `TypeIsVisible` prints a type bare
# only when the bare name finds it, so a `public` type a temporary one
# hides prints `public.t`.
@pytest.mark.backlog_fixed("RP-889")
def test_a_bare_type_name_is_found_through_the_search_path(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop schema if exists sa889 cascade")
        c.execute("create schema sa889")
        c.execute("create table sa889.st (a int)")
        c.execute("create table hid889 (b text)")
        c.execute("create temp table hid889 (a int)")
        c.execute("create temp table tt889 (a int)")
        try:
            c.execute("set search_path = sa889, public")
            got = [
                _run(c, "select 'st'::regtype::text, to_regtype('st')::text"),
                _run(c, "select (row(1)::st).a"),
            ]
            c.execute("create function sf889(st) returns int language sql as $$ select 1 $$")
            got += [
                _run(
                    c,
                    "select proargtypes::regtype[]::text from pg_proc where proname = 'sf889'",
                ),
                _run(c, "drop table st"),
            ]
            c.execute("reset search_path")
            got += [
                _run(c, "select '_tt889'::regtype::text, 'pg_temp.tt889'::regtype::text"),
                _run(c, "select (row(1)::pg_temp.tt889).a"),
                _run(c, "select 'hid889'::regtype::text, 'public.hid889'::regtype::text"),
            ]
        finally:
            c.execute("reset search_path")
            c.execute("drop table if exists tt889, hid889")
            c.execute("drop table if exists hid889")
            c.execute("drop function if exists sa889.sf889")
            c.execute("drop schema if exists sa889 cascade")
    assert got == [
        [("st", "st")],
        [(1,)],
        [("[0:0]={st}",)],
        (
            "2BP01",
            "cannot drop table st because other objects depend on it",
            "function sf889(st) depends on type st",
        ),
        [("tt889[]", "tt889")],
        [(1,)],
        [("hid889", "public.hid889")],
    ]


# `getObjectDescription` of a foreign key is `constraint <conname> on table
# <relation>`, and `conname` is the constraint's own name, never
# qualified: `pg_constraint` already has it so. A DROP's DETAIL names the
# constraint by its catalog key instead, which carries the schema of a
# table outside `public` (`s.child_b_fkey`), a temporary one's included.
@pytest.mark.backlog("RP-890")
def test_a_foreign_key_in_a_drop_detail_is_named_by_its_own_name(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop schema if exists sb890 cascade")
        c.execute("create schema sb890")
        c.execute("create table sb890.p (a int primary key)")
        c.execute("create table sb890.child (b int references sb890.p)")
        c.execute("create temp table tp890 (a int primary key)")
        c.execute("create temp table tc890 (b int references tp890)")
        try:
            got = [_run(c, "drop table sb890.p"), _run(c, "drop table tp890")]
        finally:
            c.execute("drop table if exists tc890, tp890")
            c.execute("drop schema if exists sb890 cascade")
    assert got == [
        (
            "2BP01",
            "cannot drop table sb890.p because other objects depend on it",
            "constraint child_b_fkey on table sb890.child depends on table sb890.p",
        ),
        (
            "2BP01",
            "cannot drop table tp890 because other objects depend on it",
            "constraint tc890_b_fkey on table tc890 depends on table tp890",
        ),
    ]


# The temporary schema is searched for a type ahead of `pg_catalog` too
# ("If it is not listed in the path then it is searched first (even before
# pg_catalog)"): after `create temp table text (a int)`, `'x'::text` is the
# row type's input, 22P02 `malformed record literal`, and `('(1)'::text).a`
# is 1. Here a built-in name is resolved before any user type is looked
# for, so the first answers `x` and the second is 42809.
@pytest.mark.backlog("RP-891")
def test_a_temporary_row_type_hides_a_built_in_type(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("create temp table text (a int)")
        try:
            got = [_run(c, "select ('(1)'::text).a"), _run(c, "select 'x'::text")]
        finally:
            c.execute("drop table if exists pg_temp.text")
    assert got == [
        [(1,)],
        ("22P02", 'malformed record literal: "x"', None),
    ]
