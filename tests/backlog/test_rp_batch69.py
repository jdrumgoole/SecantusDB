"""Batch 69 of the Rust PG server (2026-10-25): an UPDATE in its scan's order
(RP-247), and what RETURNING shows of the new versions (RP-248).

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch69 "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "an UPDATE returns its rows in scan order with their new ctids": (
        "RP-248",
        False,
        [
            "create table rp69_u (g int primary key, h int)",
            "insert into rp69_u select generate_series(1, 6), 0",
            "update rp69_u set h = -g where g = 2",
            "update rp69_u set h = h + 10 returning g, h, ctid",
            # A new primary key moves its row too.
            "update rp69_u set g = g + 100 where g < 3 returning g, ctid",
            "select ctid, g, h from rp69_u order by ctid",
            "drop table rp69_u",
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
    "an UPDATE returns its rows in scan order with their new ctids": [
        "CREATE TABLE",
        "INSERT 0 6",
        "UPDATE 1",
        [
            ("1", "10", "(0,8)"),
            ("3", "10", "(0,9)"),
            ("4", "10", "(0,10)"),
            ("5", "10", "(0,11)"),
            ("6", "10", "(0,12)"),
            ("2", "8", "(0,13)"),
        ],
        [("101", "(0,14)"), ("102", "(0,15)")],
        [
            ("(0,9)", "3", "10"),
            ("(0,10)", "4", "10"),
            ("(0,11)", "5", "10"),
            ("(0,12)", "6", "10"),
            ("(0,14)", "101", "10"),
            ("(0,15)", "102", "8"),
        ],
        "DROP TABLE",
    ]
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
