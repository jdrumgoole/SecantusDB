"""Batch 52 of the Rust PG server (2026-10-24): the regression tests of
RP-200 -- a block reads its own index definitions and shared comments
through `pg_get_indexdef` and `shobj_description`, and another session
does not.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch52.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario runs in
transactions it rolls back.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "a block's own definitions, and no other session's": (
        "RP-200",
        False,
        [
            "@open b",
            "begin",
            "create table rp52_t (a int)",
            "create index rp52_i on rp52_t (a)",
            "comment on database postgres is 'in a block'",
            "select pg_get_indexdef('rp52_i'::regclass)",
            "select shobj_description(oid, 'pg_database') from pg_database "
            "where datname = 'postgres'",
            "@b select count(*) from pg_indexes where indexname = 'rp52_i'",
            "@b select shobj_description(oid, 'pg_database') from pg_database "
            "where datname = 'postgres'",
            "rollback",
            "select shobj_description(oid, 'pg_database') from pg_database "
            "where datname = 'postgres'",
            "select count(*) from pg_indexes where indexname = 'rp52_i'",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a block's own definitions, and no other session's": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE INDEX",
        "COMMENT",
        [("CREATE INDEX rp52_i ON public.rp52_t USING btree (a)",)],
        [("in a block",)],
        [("0",)],
        [("default administrative connection database",)],
        "ROLLBACK",
        [("default administrative connection database",)],
        [("0",)],
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
