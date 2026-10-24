"""Batch 49 of the Rust PG server (2026-10-23): the regression tests of the
table half of RP-196 (`pg_class` shows the page and tuple counts VACUUM,
ANALYZE and CREATE INDEX record; its first test is
`test_rp_batch48.py::test_scenario[pg_class shows what VACUUM counted]`),
and the gap test of what is left of it, an index's own counts.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch49.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every table is made with
`autovacuum_enabled = false` and every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_NOAV = "with (autovacuum_enabled = false)"
_SIZES = "select relpages, reltuples, relallvisible from pg_class where relname = '{}'"

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "pg_class's counts through CREATE INDEX, ANALYZE, VACUUM and TRUNCATE": (
        "RP-196",
        False,
        [
            f"create table rp49_a (a int, t text) {_NOAV}",
            "insert into rp49_a select g, repeat('x', 100) from generate_series(1, 200) g",
            _SIZES.format("rp49_a"),
            "create index rp49_a_a on rp49_a (a)",
            _SIZES.format("rp49_a"),
            "update rp49_a set t = repeat('y', 100) where a < 50",
            "analyze rp49_a",
            _SIZES.format("rp49_a"),
            "vacuum rp49_a",
            _SIZES.format("rp49_a"),
            "truncate rp49_a",
            _SIZES.format("rp49_a"),
            "drop table rp49_a",
        ],
    ),
    "pg_class's counts of a table the planner does not size": (
        "RP-196",
        False,
        [
            f"create table rp49_n (n numeric, a int[]) {_NOAV}",
            "insert into rp49_n select g * 1.5, array[g] from generate_series(1, 300) g",
            "vacuum rp49_n",
            _SIZES.format("rp49_n"),
            "drop table rp49_n",
        ],
    ),
    "pg_class's counts of a partitioned table are its leaves'": (
        "RP-196",
        False,
        [
            "create table rp49_p (a int) partition by range (a)",
            "create table rp49_p1 partition of rp49_p for values from (0) to (100)",
            "create table rp49_p2 partition of rp49_p for values from (100) to (1000)",
            "insert into rp49_p select generate_series(0, 599)",
            "vacuum rp49_p",
            "select relname, relpages, reltuples from pg_class "
            "where relname like 'rp49_p%' order by 1",
            "drop table rp49_p",
        ],
    ),
    "pg_class shows an index's size": (
        "RP-196",
        False,
        [
            f"create table rp49_i (a int) {_NOAV}",
            "insert into rp49_i select generate_series(1, 200)",
            "create index rp49_i_a on rp49_i (a)",
            "select relpages, reltuples from pg_class where relname = 'rp49_i_a'",
            "drop table rp49_i",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "pg_class shows an index's size": [
        "CREATE TABLE",
        "INSERT 0 200",
        "CREATE INDEX",
        [("2", "200.0")],
        "DROP TABLE",
    ],
    "pg_class's counts of a partitioned table are its leaves'": [
        "CREATE TABLE",
        "CREATE TABLE",
        "CREATE TABLE",
        "INSERT 0 600",
        "VACUUM",
        [("rp49_p", "0", "-1.0"), ("rp49_p1", "1", "100.0"), ("rp49_p2", "3", "500.0")],
        "DROP TABLE",
    ],
    "pg_class's counts of a table the planner does not size": [
        "CREATE TABLE",
        "INSERT 0 300",
        "VACUUM",
        [("3", "300.0", "3")],
        "DROP TABLE",
    ],
    "pg_class's counts through CREATE INDEX, ANALYZE, VACUUM and TRUNCATE": [
        "CREATE TABLE",
        "INSERT 0 200",
        [("0", "-1.0", "0")],
        "CREATE INDEX",
        [("4", "200.0", "0")],
        "UPDATE 49",
        "ANALYZE",
        [("5", "200.0", "0")],
        "VACUUM",
        [("5", "200.0", "5")],
        "TRUNCATE TABLE",
        [("0", "-1.0", "0")],
        "DROP TABLE",
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
