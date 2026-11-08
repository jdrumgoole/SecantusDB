"""Gap tests of what was found while working RP-566 (a SQL routine over a
type that does not exist was created).

An answer is `"ok"` for a statement with no rows, or `(sqlstate, message,
position)` for an error. The expected answers were measured on a private,
freshly initdb'd PostgreSQL 15.19 cluster.
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


def _located(c: psycopg.Connection, query: str) -> Any:
    try:
        c.execute(query)
    except psycopg.Error as e:
        return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
    return "ok"


# `plpgsql_validator` compiles the body at CREATE FUNCTION
# (`check_function_bodies`), and compiling a DECLARE looks its type up
# (`parse_datatype`), so an unknown one is 42704 at the name in the body.
# The function used to be created, the error waiting for the first call.
@pytest.mark.backlog_fixed("RP-573")
def test_plpgsql_declare_of_an_undefined_type_is_refused(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            got = _located(
                c,
                "create function b573() returns int language plpgsql"
                " as $$ declare x nosuch573; begin return 1; end $$",
            )
        finally:
            c.execute("drop function if exists b573")
    assert got == ("42704", 'type "nosuch573" does not exist', "69")


def _compiled(c: psycopg.Connection, query: str) -> Any:
    try:
        c.execute(query)
    except psycopg.Error as e:
        d = e.diag
        return (e.sqlstate, d.message_primary, d.statement_position, d.context)
    return "ok"


_F = "create function d573() returns int language plpgsql as $$ {} $$"

# The relations and types the declarations below name.
DECLARE_SETUP = [
    "create table t573(a int, b text)",
    "create domain dom573 as int",
    "create schema s573",
    "create table s573.u573(z int)",
]
DECLARE_TEARDOWN = [
    "drop function if exists d573",
    "drop function if exists g573",
    "drop procedure if exists p573",
    "drop table if exists t573",
    "drop domain if exists dom573",
    "drop schema if exists s573 cascade",
]

# Each CREATE, and what PostgreSQL 15.19 answers to it: a type name is
# looked up under `parse_datatype`'s callback and its error placed at the
# type in the CREATE's text (`function_parse_error_transpose`); a
# `%ROWTYPE`'s missing relation has no position and the compiler's
# `near line N` frame, N the variable's line. A DO block compiles the
# same way before it runs.
DECLARE_CASES: list[tuple[str, Any]] = [
    (
        _F.format("declare x nosuch573[]; begin return 1; end"),
        ("42704", 'type "nosuch573[]" does not exist', "69", None),
    ),
    (
        _F.format("declare x int; y nosuch573 := 1; begin return 1; end"),
        ("42704", 'type "nosuch573" does not exist', "76", None),
    ),
    (
        _F.format("declare x constant nosuch573 := 1; begin return 1; end"),
        ("42704", 'type "nosuch573" does not exist', "78", None),
    ),
    (
        _F.format("declare x nosuch573 not null := 1; begin return 1; end"),
        ("42704", 'type "nosuch573" does not exist', "69", None),
    ),
    (
        _F.format("declare x nosch573.t; begin return 1; end"),
        ("3F000", 'schema "nosch573" does not exist', "69", None),
    ),
    (
        _F.format("declare x s573.nosuch; begin return 1; end"),
        ("42704", 'type "s573.nosuch" does not exist', "69", None),
    ),
    (
        _F.format('declare x "NoSuch573"; begin return 1; end'),
        ("42704", 'type "NoSuch573" does not exist', "69", None),
    ),
    (
        _F.format("declare x NoSuch573; begin return 1; end"),
        ("42704", 'type "nosuch573" does not exist', "69", None),
    ),
    (
        _F.format("begin declare x nosuch573; begin return 1; end; end"),
        ("42704", 'type "nosuch573" does not exist', "75", None),
    ),
    (
        _F.format("declare c cursor (a nosuch573) for select 1; begin return 1; end"),
        ("42704", 'type "nosuch573" does not exist', "79", None),
    ),
    (
        _F.format("declare x nosuch573%rowtype; begin return 1; end"),
        (
            "42P01",
            'relation "nosuch573" does not exist',
            None,
            'compilation of PL/pgSQL function "d573" near line 1',
        ),
    ),
    (
        _F.format("declare x s573.nosuch%rowtype; begin return 1; end"),
        (
            "42P01",
            'relation "s573.nosuch" does not exist',
            None,
            'compilation of PL/pgSQL function "d573" near line 1',
        ),
    ),
    (
        "create function d573() returns int language plpgsql as $$\n"
        "declare\n  x int;\n  y nosuch573\n    %rowtype;\nbegin return 1; end $$",
        (
            "42P01",
            'relation "nosuch573" does not exist',
            None,
            'compilation of PL/pgSQL function "d573" near line 4',
        ),
    ),
    (
        "create function d573() returns int language plpgsql as $$\n"
        "declare\n  x int;\n  y nosuch573;\nbegin return 1; end $$",
        ("42704", 'type "nosuch573" does not exist', "80", None),
    ),
    (
        "create function d573() returns int language plpgsql as"
        " ' declare y text := ''a''; x nosuch573; begin return 1; end '",
        ("42704", 'type "nosuch573" does not exist', "85", None),
    ),
    (
        "create function g573() returns trigger language plpgsql as $$"
        " declare x nosuch573; begin return new; end $$",
        ("42704", 'type "nosuch573" does not exist', "73", None),
    ),
    (
        "create procedure p573() language plpgsql as $$ declare x nosuch573; begin null; end $$",
        ("42704", 'type "nosuch573" does not exist', "58", None),
    ),
    (
        "do $$ declare x nosuch573; begin null; end $$",
        ("42704", 'type "nosuch573" does not exist', "17", None),
    ),
    (
        "do $$ declare x nosuch573%rowtype; begin null; end $$",
        (
            "42P01",
            'relation "nosuch573" does not exist',
            None,
            'compilation of PL/pgSQL function "inline_code_block" near line 1',
        ),
    ),
    (
        _F.format(
            "declare x t573; y dom573; z s573.u573; w t573%rowtype;"
            " v s573.u573%rowtype; u t573.a%type; o int[]; q o%type; r record;"
            " p refcursor; n pg_catalog.int4; m double precision;"
            " l timestamp with time zone; k character varying(10);"
            " j numeric(5,2); i pg_class; h pg_class%rowtype;"
            " g information_schema.tables; f dom573[]; e int not null := 1;"
            " begin return 1; end"
        ),
        "ok",
    ),
]


@pytest.mark.backlog_fixed("RP-573")
def test_plpgsql_declare_type_lookups(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for s in DECLARE_TEARDOWN + DECLARE_SETUP:
            c.execute(s)
        got = []
        try:
            for query, _ in DECLARE_CASES:
                got.append(_compiled(c, query))
                for s in DECLARE_TEARDOWN[:3]:
                    c.execute(s)
        finally:
            for s in DECLARE_TEARDOWN:
                c.execute(s)
    assert got == [expected for _, expected in DECLARE_CASES]


# `plpgsql_validator` compiles nothing when `check_function_bodies` is
# off, and a CREATE OR REPLACE the validator refuses leaves the routine it
# would have replaced.
@pytest.mark.backlog_fixed("RP-573")
def test_plpgsql_declare_check_follows_check_function_bodies(dsn: str) -> None:
    body = "declare x nosuch573; begin return 2; end"
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop function if exists d573")
        try:
            c.execute("set check_function_bodies = off")
            off = _compiled(c, _F.format(body))
            c.execute("reset check_function_bodies")
            c.execute("drop function if exists d573")
            c.execute(_F.format("begin return 1; end"))
            replaced = _compiled(c, _F.format(body).replace("create", "create or replace", 1))
            kept = c.execute("select d573()").fetchall()
        finally:
            c.execute("reset check_function_bodies")
            c.execute("drop function if exists d573")
    assert (off, replaced, kept) == (
        "ok",
        ("42704", 'type "nosuch573" does not exist', "80", None),
        [(1,)],
    )
