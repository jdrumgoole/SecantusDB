"""Batch 64 of the Rust PG server (2026-10-24): `pg_subscription` is one
catalog for the cluster (RP-169).

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch64 "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_SUB = "connection 'dbname=x' publication p with (connect = false, slot_name = none)"

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "each database names its own subscriptions": (
        "RP-169",
        False,
        # Committed, then dropped: CREATE DATABASE cannot run in a block.
        [
            "drop database if exists rp64_d1",
            "drop database if exists rp64_d2",
            "create database rp64_d1",
            "create database rp64_d2",
            "@open b dbname=rp64_d1",
            "@open c dbname=rp64_d2",
            f"@b create subscription rp64_s {_SUB}",
            f"@b create subscription rp64_t {_SUB}",
            # One name in another database; a name resolves in its own.
            f"@c create subscription rp64_s {_SUB}",
            "@c drop subscription rp64_t",
            "@c alter subscription rp64_t rename to rp64_u",
            "select subname, d.datname, o.roname is not null from pg_subscription s"
            " join pg_database d on d.oid = s.subdbid"
            " left join pg_replication_origin o on o.roname = 'pg_' || s.oid order by 1, 2",
            "select subname from pg_stat_subscription order by 1",
            "select subname from pg_stat_subscription_stats order by 1",
            "select count(*) from pg_replication_origin",
            # A subscription keeps its database.
            "!drop database rp64_d1",
            "@b drop subscription rp64_s",
            "!drop database rp64_d1",
            "@b drop subscription rp64_t",
            "@c drop subscription rp64_s",
            "@b select 1",
            "select count(*) from pg_subscription",
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
    "each database names its own subscriptions": [
        "DROP DATABASE",
        "DROP DATABASE",
        "CREATE DATABASE",
        "CREATE DATABASE",
        "CREATE SUBSCRIPTION",
        "CREATE SUBSCRIPTION",
        "CREATE SUBSCRIPTION",
        ("ERR", "42704", 'subscription "rp64_t" does not exist', None),
        ("ERR", "42704", 'subscription "rp64_t" does not exist', None),
        [
            ("rp64_s", "rp64_d1", "True"),
            ("rp64_s", "rp64_d2", "True"),
            ("rp64_t", "rp64_d1", "True"),
        ],
        [("rp64_s",), ("rp64_s",), ("rp64_t",)],
        [("rp64_s",), ("rp64_s",), ("rp64_t",)],
        [("3",)],
        (
            "ERR",
            "55006",
            'database "rp64_d1" is being used by logical replication subscription',
            None,
            "There are 2 subscriptions.",
        ),
        "DROP SUBSCRIPTION",
        (
            "ERR",
            "55006",
            'database "rp64_d1" is being used by logical replication subscription',
            None,
            "There is 1 subscription.",
        ),
        "DROP SUBSCRIPTION",
        "DROP SUBSCRIPTION",
        [("1",)],
        [("0",)],
    ]
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
