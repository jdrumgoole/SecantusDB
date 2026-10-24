"""Batch 61 of the Rust PG server (2026-10-24): a partial index whose
predicate computes (RP-236), kept as its SQL and evaluated per row.

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch61.py "host=127.0.0.1 port=5499 user=postgres"

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
    "a computed predicate's index holds only its rows": (
        "RP-236",
        False,
        [
            "create table rp61_p (a int, b int) with (autovacuum_enabled = false)",
            "create index rp61_p_b on rp61_p (b) where a % 2 = 0",
            "select indexdef from pg_indexes where indexname = 'rp61_p_b'",
            "select pg_get_expr(indpred, indrelid) from pg_index"
            " where indexrelid = 'rp61_p_b'::regclass",
            "insert into rp61_p select g, (g * 7919) % 10007 from generate_series(1, 6000) g",
            "select pg_relation_size('rp61_p_b') / 8192",
            "select count(*) from rp61_p where a % 2 = 0 and b < 100",
            "drop table rp61_p",
        ],
    ),
    "a unique partial index over a computed predicate": (
        "RP-236",
        False,
        [
            "create table rp61_u (a int, b int)",
            "insert into rp61_u values (3, 1), (4, 1), (6, 2)",
            "create unique index rp61_u_b on rp61_u (b) where a % 3 = 0",
            "!insert into rp61_u values (9, 1)",
            "insert into rp61_u values (10, 1)",
            "select count(*) from rp61_u",
            "drop table rp61_u",
        ],
    ),
    "an UPDATE into a computed unique index's rows": (
        "RP-236",
        False,
        [
            "create table rp61_w (a int, b int)",
            "insert into rp61_w values (3, 1), (10, 1), (9, null), (12, null)",
            "create unique index rp61_w_b on rp61_w (b) where a % 3 = 0",
            "!create unique index rp61_w_x on rp61_w (a) where b % 1 = 0 or a > 100",
            "!update rp61_w set a = 15 where a = 10",
            "update rp61_w set b = 5 where a = 10",
            "select indisunique from pg_index where indexrelid = 'rp61_w_b'::regclass",
            "select a, b from rp61_w order by a",
            "drop table rp61_w",
        ],
    ),
    "ON CONFLICT over a computed unique index": (
        "RP-236",
        True,
        [
            "create table rp61_c (a int, b int)",
            "create unique index rp61_c_b on rp61_c (b) where a % 3 = 0",
            "insert into rp61_c values (3, 1)",
            "insert into rp61_c values (6, 1) on conflict (b) where a % 3 = 0 do nothing",
            "insert into rp61_c values (6, 2) on conflict (b) where a % 3 = 0 do nothing",
            "select a, b from rp61_c order by a",
            "drop table rp61_c",
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
    "a computed predicate's index holds only its rows": [
        "CREATE TABLE",
        "CREATE INDEX",
        [("CREATE INDEX rp61_p_b ON public.rp61_p USING btree (b) WHERE ((a % 2) = 0)",)],
        [("((a % 2) = 0)",)],
        "INSERT 0 6000",
        [("11",)],
        [("29",)],
        "DROP TABLE",
    ],
    "a unique partial index over a computed predicate": [
        "CREATE TABLE",
        "INSERT 0 3",
        "CREATE INDEX",
        (
            "ERR",
            "23505",
            'duplicate key value violates unique constraint "rp61_u_b"',
            None,
            "Key (b)=(1) already exists.",
        ),
        "INSERT 0 1",
        [("4",)],
        "DROP TABLE",
    ],
    "an UPDATE into a computed unique index's rows": [
        "CREATE TABLE",
        "INSERT 0 4",
        "CREATE INDEX",
        "CREATE INDEX",
        (
            "ERR",
            "23505",
            'duplicate key value violates unique constraint "rp61_w_b"',
            None,
            "Key (b)=(1) already exists.",
        ),
        "UPDATE 1",
        [("True",)],
        [("3", "1"), ("9", "None"), ("10", "5"), ("12", "None")],
        "DROP TABLE",
    ],
    "ON CONFLICT over a computed unique index": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 1",
        "INSERT 0 0",
        "INSERT 0 1",
        [("3", "1"), ("6", "2")],
        "DROP TABLE",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
