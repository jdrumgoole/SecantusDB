"""Batch 44 of the Rust PG server (2026-10-23): the regression test of the
rest of RP-178 -- citext's byte-order operators (`~<~` and the rest) over
casts and columns, and comparisons named `OPERATOR(s.op)` with citext's
schema or `pg_catalog`. Its first test is in `test_rp_batch38.py`.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch44.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "citext's byte-order and schema-named operators": (
        "RP-178",
        False,
        [
            "create extension citext",
            "select 'a'::citext ~<~ 'B'::citext, 'a'::citext ~>=~ 'B'::citext,"
            " 'B'::citext ~<=~ 'b'::citext, 'b'::citext ~>~ 'A'::citext",
            "create table rp44_c (c citext, t text)",
            "insert into rp44_c values ('a', 'a'), ('B', 'B')",
            "select c::text from rp44_c where c ~<~ 'b' order by c",
            "select t from rp44_c where t ~<~ 'b' order by t",
            "select 'A'::citext operator(public.=) 'a'::citext,"
            " 'A'::citext operator(pg_catalog.=) 'a'::citext",
            "select c::text from rp44_c where c operator(pg_catalog.=) 'b'",
            "select c::text from rp44_c where c operator(public.=) 'b'",
            "drop table rp44_c",
            "drop extension citext",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "citext's byte-order and schema-named operators": [
        "CREATE EXTENSION",
        [("True", "False", "True", "True")],
        "CREATE TABLE",
        "INSERT 0 2",
        [("a",)],
        [("B",), ("a",)],
        [("True", "False")],
        [],
        [("B",)],
        "DROP TABLE",
        "DROP EXTENSION",
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
