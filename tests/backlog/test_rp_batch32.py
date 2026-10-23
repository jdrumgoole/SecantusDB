"""Batch 32 of the Rust PG server (2026-10-23): the regression tests of
what it fixed -- RP-072's `ALTER TYPE ... ADD / DROP / ALTER ATTRIBUTE`,
and RP-156 (typed tables) and RP-158 (a dropped composite field's stored
values), found while building it -- and the gap test of what it filed
open (RP-157).

As in `test_rp_batch31.py`: each scenario is a list of statements run in
one autocommit connection, and a statement's answer is its rows, its
command tag, or `("ERR", sqlstate)`. A statement prefixed `!` reports its
error's DETAIL too; one prefixed `~` reports its answer with the notices
it raised. The expected answers were measured on a private, freshly
initdb'd PostgreSQL 15.19 cluster by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch32.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch31 import run_scenario

# (item, open?, statements). `open` scenarios are gap tests: they fail while
# their item is open.
SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------------------- RP-072 ALTER TYPE attributes
    "ALTER TYPE adds and drops attributes of a type in use": (
        "RP-072",
        False,
        [
            "create type rp32_ct as (a int, b text)",
            "create table rp32_ctt (v rp32_ct, w rp32_ct[])",
            "insert into rp32_ctt values (row(1, 'x'), array[row(5, 'p')::rp32_ct])",
            "alter type rp32_ct alter attribute a type bigint",
            "alter type rp32_ct add attribute c int",
            "select v::text, (v).c, (v).b, w::text from rp32_ctt",
            "insert into rp32_ctt values (row(2, 'y', 3), null)",
            "alter type rp32_ct drop attribute a",
            "select v::text, (v).b, (v).c, w::text from rp32_ctt order by (v).b",
            "select (v).a from rp32_ctt",
            "insert into rp32_ctt values (row('z', 4), null)",
            "select v::text from rp32_ctt order by (v).b",
            "select attname, attnum, attisdropped from pg_attribute "
            "where attrelid = 'rp32_ct'::regclass order by attnum",
            "drop table rp32_ctt",
            "alter type rp32_ct alter attribute c type text",
            "select '(q,r)'::rp32_ct::text",
            "drop type rp32_ct",
        ],
    ),
    "ALTER TYPE attribute errors": (
        "RP-072",
        False,
        [
            "create type rp32_ct as (a int)",
            "~alter type rp32_ct drop attribute if exists nosuch",
            "alter type rp32_ct drop attribute nosuch",
            "alter type rp32_ct add attribute a int",
            "alter type rp32_ct add attribute z rp32_nosuchtype",
            "alter type rp32_ct alter attribute nosuch type int",
            "alter type rp32_nosuch add attribute c int",
            "create type rp32_e as enum ('a')",
            "alter type rp32_e add attribute c int",
            "create table rp32_t (x int)",
            "alter type rp32_t add attribute y int",
            "alter type rp32_ct drop attribute a",
            "alter type rp32_ct add attribute b text, add attribute c int",
            "select attname, attnum from pg_attribute "
            "where attrelid = 'rp32_ct'::regclass order by attnum",
            "drop table rp32_t",
            "drop type rp32_ct, rp32_e",
        ],
    ),
    "ALTER TYPE ... CASCADE alters the typed tables": (
        "RP-072",
        False,
        [
            "create type rp32_ct as (a int, b text)",
            "create table rp32_typed of rp32_ct (b with options not null)",
            "insert into rp32_typed values (1, 'l')",
            "!alter type rp32_ct add attribute d date",
            "alter type rp32_ct add attribute d date cascade",
            "alter type rp32_ct drop attribute b cascade",
            "alter type rp32_ct rename attribute a to aa",
            "alter type rp32_ct rename attribute a to aa cascade",
            "select * from rp32_typed",
            "!drop type rp32_ct",
            "~drop type rp32_ct cascade",
            "select count(*) from pg_class where relname = 'rp32_typed'",
        ],
    ),
    # --------------------------------------------------- RP-156 typed tables
    "CREATE TABLE OF a type takes its attributes": (
        "RP-156",
        False,
        [
            "create type rp32_ct as (a int, b text)",
            "create table rp32_typed of rp32_ct (a with options not null, primary key (a))",
            "select attname, atttypid::regtype::text, attnotnull from pg_attribute "
            "where attrelid = 'rp32_typed'::regclass and attnum > 0 order by attnum",
            "select reloftype::regtype::text from pg_class where relname = 'rp32_typed'",
            "insert into rp32_typed values (1, 'x')",
            "insert into rp32_typed values (1, 'y')",
            "select * from rp32_typed",
            "alter table rp32_typed add column z int",
            "alter table rp32_typed drop column b",
            "alter table rp32_typed rename column b to bb",
            "create table rp32_typed2 of rp32_nosuch",
            "create table rp32_typed3 of int4",
            "create table rp32_typed4 of rp32_ct (nosuch with options not null)",
            "drop table rp32_typed",
            "drop type rp32_ct",
        ],
    ),
    "ALTER TABLE OF and NOT OF": (
        "RP-156",
        False,
        [
            "create type rp32_ct as (a int, b text)",
            "create table rp32_typed of rp32_ct",
            "alter table rp32_typed not of",
            "select reloftype from pg_class where relname = 'rp32_typed'",
            "alter table rp32_typed add column z int",
            "create table rp32_plain (a int, b text)",
            "alter table rp32_plain of rp32_ct",
            "select reloftype::regtype::text from pg_class where relname = 'rp32_plain'",
            "alter table rp32_plain add column z int",
            "create table rp32_x1 (a int)",
            "alter table rp32_x1 of rp32_ct",
            "create table rp32_x2 (a int, c text)",
            "alter table rp32_x2 of rp32_ct",
            "create table rp32_x3 (a int, b int)",
            "alter table rp32_x3 of rp32_ct",
            "create table rp32_x4 (a int, b text, z int)",
            "alter table rp32_x4 of rp32_ct",
            "alter table rp32_x4 of int4",
            "alter table rp32_x4 not of",
            "drop table rp32_typed, rp32_plain, rp32_x1, rp32_x2, rp32_x3, rp32_x4",
            "drop type rp32_ct",
        ],
    ),
    # --------------------- RP-158 a dropped composite field's stored values
    "DROP TYPE CASCADE drops a composite's field from its values": (
        "RP-158",
        False,
        [
            "create type rp32_e as enum ('p', 'q')",
            "create type rp32_c2 as (x int, e rp32_e, y text)",
            "create table rp32_u (v rp32_c2)",
            "insert into rp32_u values (row(1, 'p', 'k'))",
            "~drop type rp32_e cascade",
            "select v::text, (v).y from rp32_u",
            "select attname, attnum, attisdropped from pg_attribute "
            "where attrelid = 'rp32_c2'::regclass order by attnum",
            "drop table rp32_u",
            "drop type rp32_c2",
        ],
    ),
    # ---------------------------------------------------- open: filed here
    "a dropped table column leaves its attnum": (
        "RP-157",
        True,
        [
            "create table rp32_d (a int, b text, c int)",
            "alter table rp32_d drop column b",
            "select attname, attnum, attisdropped from pg_attribute "
            "where attrelid = 'rp32_d'::regclass and attnum > 0 order by attnum",
            "select column_name, ordinal_position from information_schema.columns "
            "where table_name = 'rp32_d' order by ordinal_position",
            "drop table rp32_d",
        ],
    ),
}


def _scenario(name: str, server: Any) -> list[Any]:
    return run_scenario(_dsn(server) + " dbname=postgres", SCENARIOS[name][2])


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    assert _scenario(name, rust_pg) == EXPECTED[name]


EXPECTED: dict[str, Any] = {
    "ALTER TABLE OF and NOT OF": [
        "CREATE TYPE",
        "CREATE TABLE",
        "ALTER TABLE",
        [("0",)],
        "ALTER TABLE",
        "CREATE TABLE",
        "ALTER TABLE",
        [("rp32_ct",)],
        ("ERR", "42809"),
        "CREATE TABLE",
        ("ERR", "42804"),
        "CREATE TABLE",
        ("ERR", "42804"),
        "CREATE TABLE",
        ("ERR", "42804"),
        "CREATE TABLE",
        ("ERR", "42804"),
        ("ERR", "42809"),
        ("ERR", "42809"),
        "DROP TABLE",
        "DROP TYPE",
    ],
    "ALTER TYPE ... CASCADE alters the typed tables": [
        "CREATE TYPE",
        "CREATE TABLE",
        "INSERT 0 1",
        ("ERR", "2BP01", None),
        "ALTER TYPE",
        "ALTER TYPE",
        ("ERR", "2BP01"),
        "ALTER TYPE",
        [("1", "None")],
        ("ERR", "2BP01", "table rp32_typed depends on type rp32_ct"),
        ("DROP TYPE", ["drop cascades to table rp32_typed"]),
        [("0",)],
    ],
    "ALTER TYPE adds and drops attributes of a type in use": [
        "CREATE TYPE",
        "CREATE TABLE",
        "INSERT 0 1",
        ("ERR", "0A000"),
        "ALTER TYPE",
        [("(1,x,)", "None", "x", '{"(5,p,)"}')],
        "INSERT 0 1",
        "ALTER TYPE",
        [("(x,)", "x", "None", '{"(p,)"}'), ("(y,3)", "y", "3", "None")],
        ("ERR", "42703"),
        "INSERT 0 1",
        [("(x,)",), ("(y,3)",), ("(z,4)",)],
        [("........pg.dropped.1........", "1", "True"), ("b", "2", "False"), ("c", "3", "False")],
        "DROP TABLE",
        "ALTER TYPE",
        [("(q,r)",)],
        "DROP TYPE",
    ],
    "ALTER TYPE attribute errors": [
        "CREATE TYPE",
        ("ALTER TYPE", ['column "nosuch" of relation "rp32_ct" does not exist, skipping']),
        ("ERR", "42703"),
        ("ERR", "42701"),
        ("ERR", "42704"),
        ("ERR", "42703"),
        ("ERR", "42P01"),
        "CREATE TYPE",
        ("ERR", "42P01"),
        "CREATE TABLE",
        ("ERR", "42809"),
        "ALTER TYPE",
        "ALTER TYPE",
        [("........pg.dropped.1........", "1"), ("b", "2"), ("c", "3")],
        "DROP TABLE",
        "DROP TYPE",
    ],
    "CREATE TABLE OF a type takes its attributes": [
        "CREATE TYPE",
        "CREATE TABLE",
        [("a", "integer", "True"), ("b", "text", "False")],
        [("rp32_ct",)],
        "INSERT 0 1",
        ("ERR", "23505"),
        [("1", "x")],
        ("ERR", "42809"),
        ("ERR", "42809"),
        ("ERR", "42809"),
        ("ERR", "42704"),
        ("ERR", "42809"),
        ("ERR", "42703"),
        "DROP TABLE",
        "DROP TYPE",
    ],
    "DROP TYPE CASCADE drops a composite's field from its values": [
        "CREATE TYPE",
        "CREATE TYPE",
        "CREATE TABLE",
        "INSERT 0 1",
        ("DROP TYPE", ["drop cascades to column e of composite type rp32_c2"]),
        [("(1,k)", "k")],
        [("x", "1", "False"), ("........pg.dropped.2........", "2", "True"), ("y", "3", "False")],
        "DROP TABLE",
        "DROP TYPE",
    ],
    "a dropped table column leaves its attnum": [
        "CREATE TABLE",
        "ALTER TABLE",
        [("a", "1", "False"), ("........pg.dropped.2........", "2", "True"), ("c", "3", "False")],
        [("a", "1"), ("c", "3")],
        "DROP TABLE",
    ],
}


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1]
    measured = {
        name: run_scenario(dsn + " dbname=postgres", stmts)
        for name, (_, _, stmts) in SCENARIOS.items()
    }
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
