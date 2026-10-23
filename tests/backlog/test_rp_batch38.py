"""Batch 38 of the Rust PG server (2026-10-23): the regression tests of
what it fixed -- RP-172 (an extension's objects are its schema's), and,
found beside it, RP-176 (a user type's unqualified name is found through
the search path), RP-177 (`ALTER TYPE / DOMAIN / EXTENSION ... SET
SCHEMA`), RP-179 (DROP SCHEMA CASCADE left its domains) and RP-181 (a
column of an array of another schema's type was missing from
`pg_attribute`) -- and the gap
tests of RP-178 and RP-180, found with them.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch38.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------- RP-172 an extension's objects are its schema's
    "an extension's functions follow the search path": (
        "RP-172",
        False,
        [
            "create extension pg_trgm",
            "create extension pgcrypto",
            "select public.similarity('ab', 'ab')",
            "select pg_catalog.similarity('ab', 'ab')",
            "select length(public.digest('a', 'md5'))",
            "set search_path = pg_catalog",
            "select similarity('ab', 'ab')",
            "select digest('a', 'md5')",
            "select 'ab'::text % 'ab'::text",
            "reset search_path",
            "select similarity('ab', 'ab'), 'ab' % 'ab'",
            "drop extension pg_trgm",
            "drop extension pgcrypto",
        ],
    ),
    "an extension's type is its schema's": (
        "RP-172",
        False,
        [
            "create schema rp38_e",
            "create extension citext schema rp38_e",
            "select pg_typeof('A'::rp38_e.citext)::text,"
            " format_type('rp38_e.citext'::regtype, null)",
            "select 'A'::citext",
            "create table rp38_t (c rp38_e.citext)",
            "insert into rp38_t values ('A')",
            "select count(*) from rp38_t where c = 'a'",
            "select format_type(atttypid, atttypmod) from pg_attribute"
            " where attrelid = 'rp38_t'::regclass and attname = 'c'",
            "set search_path = rp38_e, public",
            "select 'A'::citext = 'a'::citext",
            "select count(*) from rp38_t where c = 'a'",
            "reset search_path",
            "drop table rp38_t",
            "drop extension citext",
            "create extension citext schema rp38_nosuch",
            "drop schema rp38_e",
        ],
    ),
    "an extension off the path": (
        "RP-172",
        False,
        [
            "create extension citext",
            "set search_path = pg_catalog",
            "select 'A'::citext",
            "select 'A'::public.citext = 'a'::public.citext",
            "select pg_typeof('A'::public.citext)::text",
            "reset search_path",
            "select 'A'::citext = 'a'::citext",
            "drop extension citext",
        ],
    ),
    # --------------------- RP-176 a user type is found through the search path
    "a user type is found through the search path": (
        "RP-176",
        False,
        [
            "create schema rp38_q",
            "create type rp38_q.mood as enum ('a')",
            "create type public.rp38_pmood as enum ('p')",
            "set search_path = rp38_q, public",
            "select 'a'::mood, pg_typeof('a'::mood)::text",
            "create table rp38_qt (m mood)",
            "insert into rp38_qt values ('a')",
            "select m, pg_typeof(m)::text from rp38_qt",
            "select format_type(atttypid, null) from pg_attribute"
            " where attrelid = 'rp38_qt'::regclass and attname = 'm'",
            "set search_path = pg_catalog",
            "select 'p'::rp38_pmood",
            "select 'p'::public.rp38_pmood",
            "reset search_path",
            "drop table rp38_q.rp38_qt",
            "drop schema rp38_q cascade",
            "drop type rp38_pmood",
        ],
    ),
    # ------------------- RP-177 ALTER TYPE | DOMAIN | EXTENSION ... SET SCHEMA
    "ALTER EXTENSION SET SCHEMA": (
        "RP-177",
        False,
        [
            "create schema rp38_m",
            "create extension citext schema rp38_m",
            "create table rp38_mt (c rp38_m.citext)",
            "insert into rp38_mt values ('A')",
            "alter extension citext set schema public",
            "select extnamespace::regnamespace::text from pg_extension where extname = 'citext'",
            "select 'A'::citext = 'a'::citext, pg_typeof(c)::text, c = 'a' from rp38_mt",
            "alter extension citext set schema public",
            "alter extension citext set schema rp38_nosuch",
            "alter extension rp38_nosuch set schema public",
            "alter extension plpgsql set schema public",
            "drop table rp38_mt",
            "drop extension citext",
            "drop schema rp38_m",
        ],
    ),
    "ALTER TYPE and ALTER DOMAIN SET SCHEMA": (
        "RP-177",
        False,
        [
            "create schema rp38_y",
            "create schema rp38_z",
            "create type rp38_y.m as enum ('a')",
            "create domain rp38_y.d as int",
            "create table rp38_yt (c rp38_y.m, d rp38_y.m[], e rp38_y.d)",
            "insert into rp38_yt values ('a', '{a}', 1)",
            "alter type rp38_y.m set schema rp38_y",
            "alter type rp38_y.m set schema rp38_nosuch",
            "alter type rp38_y.m set schema rp38_z",
            "alter domain rp38_y.d set schema rp38_z",
            "select c::text, pg_typeof(c)::text, pg_typeof(d)::text, pg_typeof(e)::text"
            " from rp38_yt",
            "select format_type(atttypid, null) from pg_attribute"
            " where attrelid = 'rp38_yt'::regclass and attnum > 0 order by attnum",
            "create type rp38_y.m as enum ('b')",
            "alter type rp38_y.m set schema rp38_z",
            "drop table rp38_yt",
            "drop schema rp38_y, rp38_z cascade",
        ],
    ),
    # --------------------------- RP-179 DROP SCHEMA ... CASCADE and its domains
    "DROP SCHEMA CASCADE drops its domains": (
        "RP-179",
        False,
        [
            "create schema rp38_w",
            "create domain rp38_w.d as int",
            "drop schema rp38_w cascade",
            "create schema rp38_w",
            "create domain rp38_w.d as int",
            "select 1::rp38_w.d",
            "drop schema rp38_w cascade",
        ],
    ),
    # ---------------- RP-181 a column of an array of a schema's type is listed
    "an array of a schema's type is a column": (
        "RP-181",
        False,
        [
            "create schema rp38_a",
            "create type rp38_a.m as enum ('a')",
            "create table rp38_at (d rp38_a.m[])",
            "select attname, format_type(atttypid, null) from pg_attribute"
            " where attrelid = 'rp38_at'::regclass and attnum > 0",
            "drop table rp38_at",
            "drop schema rp38_a cascade",
        ],
    ),
    # ------------------------ RP-180 a missing qualified type is named in full
    "a missing type named with its schema": (
        "RP-180",
        True,
        [
            "create schema rp38_v",
            "alter type rp38_v.nosuch set schema public",
            "alter type rp38_v.nosuch rename to x",
            "alter type public.nosuch rename to x",
            "drop schema rp38_v",
        ],
    ),
    # ------------------- RP-178 citext's own functions and qualified operators
    "citext's functions and an operator named by its schema": (
        "RP-178",
        True,
        [
            "create schema rp38_c",
            "create extension citext schema rp38_c",
            "select 'A'::rp38_c.citext operator(rp38_c.=) 'a'::rp38_c.citext",
            "select rp38_c.citext('X')::text, pg_typeof(rp38_c.citext('X'))::text",
            "select rp38_c.citext(true)::text",
            "drop extension citext",
            "drop schema rp38_c",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "ALTER EXTENSION SET SCHEMA": [
        "CREATE SCHEMA",
        "CREATE EXTENSION",
        "CREATE TABLE",
        "INSERT 0 1",
        "ALTER EXTENSION",
        [("public",)],
        [("True", "citext", "True")],
        "ALTER EXTENSION",
        ("ERR", "3F000", 'schema "rp38_nosuch" does not exist', None),
        ("ERR", "42704", 'extension "rp38_nosuch" does not exist', None),
        ("ERR", "0A000", 'extension "plpgsql" does not support SET SCHEMA', None),
        "DROP TABLE",
        "DROP EXTENSION",
        "DROP SCHEMA",
    ],
    "ALTER TYPE and ALTER DOMAIN SET SCHEMA": [
        "CREATE SCHEMA",
        "CREATE SCHEMA",
        "CREATE TYPE",
        "CREATE DOMAIN",
        "CREATE TABLE",
        "INSERT 0 1",
        "ALTER TYPE",
        ("ERR", "3F000", 'schema "rp38_nosuch" does not exist', None),
        "ALTER TYPE",
        "ALTER DOMAIN",
        [("a", "rp38_z.m", "rp38_z.m[]", "rp38_z.d")],
        [("rp38_z.m",), ("rp38_z.m[]",), ("rp38_z.d",)],
        "CREATE TYPE",
        ("ERR", "42710", 'type "m" already exists in schema "rp38_z"', None),
        "DROP TABLE",
        "DROP SCHEMA",
    ],
    "DROP SCHEMA CASCADE drops its domains": [
        "CREATE SCHEMA",
        "CREATE DOMAIN",
        "DROP SCHEMA",
        "CREATE SCHEMA",
        "CREATE DOMAIN",
        [("1",)],
        "DROP SCHEMA",
    ],
    "a missing type named with its schema": [
        "CREATE SCHEMA",
        ("ERR", "42704", 'type "rp38_v.nosuch" does not exist', None),
        ("ERR", "42704", 'type "rp38_v.nosuch" does not exist', None),
        ("ERR", "42704", 'type "public.nosuch" does not exist', None),
        "DROP SCHEMA",
    ],
    "a user type is found through the search path": [
        "CREATE SCHEMA",
        "CREATE TYPE",
        "CREATE TYPE",
        "SET",
        [("a", "mood")],
        "CREATE TABLE",
        "INSERT 0 1",
        [("a", "mood")],
        [("mood",)],
        "SET",
        ("ERR", "42704", 'type "rp38_pmood" does not exist', None),
        [("p",)],
        "RESET",
        "DROP TABLE",
        "DROP SCHEMA",
        "DROP TYPE",
    ],
    "an array of a schema's type is a column": [
        "CREATE SCHEMA",
        "CREATE TYPE",
        "CREATE TABLE",
        [("d", "rp38_a.m[]")],
        "DROP TABLE",
        "DROP SCHEMA",
    ],
    "an extension off the path": [
        "CREATE EXTENSION",
        "SET",
        ("ERR", "42704", 'type "citext" does not exist', None),
        [("False",)],
        [("public.citext",)],
        "RESET",
        [("True",)],
        "DROP EXTENSION",
    ],
    "an extension's functions follow the search path": [
        "CREATE EXTENSION",
        "CREATE EXTENSION",
        [("1.0",)],
        (
            "ERR",
            "42883",
            "function pg_catalog.similarity(unknown, unknown) does not exist",
            "No function matches the given name and "
            "argument types. You might need to add "
            "explicit type casts.",
        ),
        [("16",)],
        "SET",
        (
            "ERR",
            "42883",
            "function similarity(unknown, unknown) does not exist",
            "No function matches the given name and "
            "argument types. You might need to add "
            "explicit type casts.",
        ),
        (
            "ERR",
            "42883",
            "function digest(unknown, unknown) does not exist",
            "No function matches the given name and "
            "argument types. You might need to add "
            "explicit type casts.",
        ),
        (
            "ERR",
            "42883",
            "operator does not exist: text % text",
            "No operator matches the given name and "
            "argument types. You might need to add "
            "explicit type casts.",
        ),
        "RESET",
        [("1.0", "True")],
        "DROP EXTENSION",
        "DROP EXTENSION",
    ],
    "an extension's type is its schema's": [
        "CREATE SCHEMA",
        "CREATE EXTENSION",
        [("rp38_e.citext", "rp38_e.citext")],
        ("ERR", "42704", 'type "citext" does not exist', None),
        "CREATE TABLE",
        "INSERT 0 1",
        [("0",)],
        [("rp38_e.citext",)],
        "SET",
        [("True",)],
        [("1",)],
        "RESET",
        "DROP TABLE",
        "DROP EXTENSION",
        ("ERR", "3F000", 'schema "rp38_nosuch" does not exist', None),
        "DROP SCHEMA",
    ],
    "citext's functions and an operator named by its schema": [
        "CREATE SCHEMA",
        "CREATE EXTENSION",
        [("True",)],
        [("X", "rp38_c.citext")],
        [("true",)],
        "DROP EXTENSION",
        "DROP SCHEMA",
    ],
}


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    got = run_scenario(_dsn(rust_pg) + " dbname=postgres", SCENARIOS[name][2])
    assert got == EXPECTED[name]


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1]
    measured = {
        name: run_scenario(dsn + " dbname=postgres", stmts)
        for name, (_, _, stmts) in SCENARIOS.items()
    }
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
