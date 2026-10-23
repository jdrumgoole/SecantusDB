"""Batch 36 of the Rust PG server (2026-10-23): the regression tests of
what it fixed -- RP-149 (a built-in reached through a schema), RP-150 (DROP
SCHEMA's refusal names what depends on its members), RP-154 (a whole
record in a PL/pgSQL expression), and RP-174 (an `information_schema`
helper's column name), found while fixing them -- and the gap tests of what
it filed open (RP-172, RP-173).

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch36.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------------ RP-154 a whole record, more ways
    "a whole record in PL/pgSQL expressions": (
        "RP-154",
        False,
        [
            "create table rp36_t (a int, b text)",
            "insert into rp36_t values (1, 'q'), (2, null)",
            "~do $$ declare r record; s record; x text; begin "
            "select * into r from rp36_t where a = 1; "
            "raise notice '%', r; "
            "x := r::text; raise notice 'text %', x; "
            "raise notice 'isnull % notnull %', r is null, r is not null; "
            "select * into s from rp36_t where a = 2; "
            "raise notice 'eq % ne %', r = s, r <> s; "
            "raise notice 'concat %', 'x' || r::text; "
            "select * into s from rp36_t where a = 99; "
            "raise notice 'empty % isnull %', s, s is null; "
            "end $$",
            "~do $$ declare r record; x text; begin x := r::text; "
            "raise notice 'x=% isnull=%', x, r is null; raise notice 'v %', r; end $$",
            "drop table rp36_t",
        ],
    ),
    # --------------------------------- RP-149 built-ins through a schema
    "a built-in through a schema, anywhere in a statement": (
        "RP-149",
        False,
        [
            "create schema rp36_n",
            "select rp36_n.lower(x) from (values ('a')) v(x)",
            "select public.now()",
            "select * from public.generate_series(1, 2)",
            "select 1 where public.length('ab') = 2",
            "select pg_catalog.lower('A'), pg_catalog.upper(lower('B'))",
            "select information_schema._pg_char_max_length(1043, 14)",
            "create function rp36_n.lower(int) returns int language sql as 'select 7'",
            "select rp36_n.lower(1)",
            "drop schema rp36_n cascade",
        ],
    ),
    # ------------------------- RP-150 what depends on a schema's members
    "DROP SCHEMA's refusal nests what depends on each member": (
        "RP-150",
        False,
        [
            "create schema rp36_o",
            "create table rp36_o.t (a int)",
            "create view rp36_v1 as select * from rp36_o.t",
            "create view rp36_v2 as select * from rp36_v1",
            "!drop schema rp36_o",
            "~drop schema rp36_o cascade",
        ],
    ),
    # ------------------ RP-174 an information_schema helper's column name
    "an information_schema helper names its column": (
        "RP-174",
        False,
        [
            "select information_schema._pg_char_max_length(1043, 14), "
            "information_schema._pg_numeric_precision(23, -1)",
            "select 1 + information_schema._pg_char_max_length(1043, 14), "
            "information_schema._pg_char_max_length(1043, 14) as x",
        ],
    ),
    # ----------------------- RP-172 CREATE EXTENSION ... SCHEMA is ignored
    "an extension installed in a schema": (
        "RP-172",
        True,
        [
            "create schema rp36_e",
            "create extension citext schema rp36_e",
            "select extnamespace::regnamespace::text from pg_extension where extname = 'citext'",
            "select typnamespace::regnamespace::text from pg_type where typname = 'citext'",
            "select 'A'::rp36_e.citext = 'a'::rp36_e.citext",
            "drop extension citext",
            "drop schema rp36_e",
        ],
    ),
    # ---------------- RP-173 a user function of a built-in's name hides it
    "a user function of a built-in's name": (
        "RP-173",
        True,
        [
            "create function public.lower(int) returns int language sql as 'select 1'",
            "create function public.abs(int) returns int language sql as 'select 99'",
            "select lower('Q'), lower(5), pg_catalog.lower('Q'), abs(-3), public.abs(-3)",
            "select pg_catalog.lower(5)",
            "drop function public.lower(int)",
            "drop function public.abs(int)",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "DROP SCHEMA's refusal nests what depends on each member": [
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE VIEW",
        "CREATE VIEW",
        (
            "ERR",
            "2BP01",
            "cannot drop schema rp36_o because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "table rp36_o.t depends on schema "
            "rp36_o\n"
            "view rp36_v1 depends on table "
            "rp36_o.t\n"
            "view rp36_v2 depends on view "
            "rp36_v1",
        ),
        (
            "DROP SCHEMA",
            [
                "drop cascades to 3 other objects\n"
                "drop cascades to table rp36_o.t\n"
                "drop cascades to view rp36_v1\n"
                "drop cascades to view rp36_v2"
            ],
        ),
    ],
    "a built-in through a schema, anywhere in a statement": [
        "CREATE SCHEMA",
        (
            "ERR",
            "42883",
            "function rp36_n.lower(text) does not exist",
            "No function matches the given name and "
            "argument types. You might need to add "
            "explicit type casts.",
        ),
        (
            "ERR",
            "42883",
            "function public.now() does not exist",
            "No function matches the given name and "
            "argument types. You might need to add "
            "explicit type casts.",
        ),
        (
            "ERR",
            "42883",
            "function public.generate_series(integer, integer) does not exist",
            "No function matches the given name and "
            "argument types. You might need to add "
            "explicit type casts.",
        ),
        (
            "ERR",
            "42883",
            "function public.length(unknown) does not exist",
            "No function matches the given name and "
            "argument types. You might need to add "
            "explicit type casts.",
        ),
        [("a", "B")],
        [("10",)],
        "CREATE FUNCTION",
        [("7",)],
        "DROP SCHEMA",
    ],
    "a user function of a built-in's name": [
        "CREATE FUNCTION",
        "CREATE FUNCTION",
        [("q", "1", "q", "3", "99")],
        (
            "ERR",
            "42883",
            "function pg_catalog.lower(integer) does not exist",
            "No function matches the given name and argument types. "
            "You might need to add explicit type casts.",
        ),
        "DROP FUNCTION",
        "DROP FUNCTION",
    ],
    "a whole record in PL/pgSQL expressions": [
        "CREATE TABLE",
        "INSERT 0 2",
        (
            "DO",
            [
                "(1,q)",
                "text (1,q)",
                "isnull f notnull t",
                "eq f ne t",
                "concat x(1,q)",
                "empty (,) isnull t",
            ],
        ),
        ("DO", ["x=<NULL> isnull=t", "v <NULL>"]),
        "DROP TABLE",
    ],
    "an extension installed in a schema": [
        "CREATE SCHEMA",
        "CREATE EXTENSION",
        [("rp36_e",)],
        [("rp36_e",)],
        [("False",)],
        "DROP EXTENSION",
        "DROP SCHEMA",
    ],
    "an information_schema helper names its column": [[("10", "32")], [("11", "10")]],
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
