"""Batch 43 of the Rust PG server (2026-10-23): the regression test of
RP-173 beyond its first one (in `test_rp_batch36.py`) -- a user function
named like a built-in is a candidate beside the built-in, not a
replacement for it: over columns, with the path placing its schema first,
and in a view.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch43.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "a user function of a built-in's name over columns and the path": (
        "RP-173",
        False,
        [
            "create function public.lower(int) returns int language sql as 'select 1'",
            "create function public.abs(int) returns int language sql as 'select 99'",
            "create function public.upper(text) returns text language sql as 'select ''U'''",
            "create table rp43_t (i int, t text)",
            "insert into rp43_t values (5, 'Q')",
            "select lower(i), lower(t), abs(i), lower(i::int), lower(t::text) from rp43_t",
            "select upper('a'), upper('a'::text), public.upper('a')",
            "set search_path = public, pg_catalog",
            "select upper('a'::text), abs(-3), lower(5)",
            "reset search_path",
            "create view rp43_v as select lower(5) as a, lower('Q') as b",
            "select * from rp43_v",
            "drop view rp43_v",
            "drop table rp43_t",
            "drop function public.upper(text)",
            "drop function public.lower(int)",
            "drop function public.abs(int)",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a user function of a built-in's name over columns and the path": [
        "CREATE FUNCTION",
        "CREATE FUNCTION",
        "CREATE FUNCTION",
        "CREATE TABLE",
        "INSERT 0 1",
        [("1", "q", "5", "1", "q")],
        [("A", "A", "U")],
        "SET",
        [("U", "99", "1")],
        "RESET",
        "CREATE VIEW",
        [("1", "q")],
        "DROP VIEW",
        "DROP TABLE",
        "DROP FUNCTION",
        "DROP FUNCTION",
        "DROP FUNCTION",
    ]
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
