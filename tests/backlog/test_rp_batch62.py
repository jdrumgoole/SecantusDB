"""Batch 62 of the Rust PG server (2026-10-24): ON CONFLICT over a unique
index whose predicate computes (RP-236), and a DO UPDATE that would update
one row twice (RP-237).

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch62 "host=127.0.0.1 port=5499 user=postgres"

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
    "ON CONFLICT DO UPDATE over a computed unique index": (
        "RP-236",
        False,
        [
            "create table rp62_u (a int, b int, n int default 0)",
            "create unique index rp62_u_b on rp62_u (b) where a % 3 = 0",
            "insert into rp62_u values (3, 1), (4, 1)",
            # Admitted and conflicting, then a row the predicate does not
            # admit.
            "insert into rp62_u values (6, 1) on conflict (b) where a % 3 = 0"
            " do update set n = rp62_u.n + 1, a = excluded.a returning a, b, n",
            "insert into rp62_u values (5, 1) on conflict (b) where a % 3 = 0"
            " do update set n = 100 returning a, b, n",
            # A NULL key never conflicts.
            "insert into rp62_u values (15, null), (18, null) on conflict (b)"
            " where a % 3 = 0 do nothing",
            # The update that would make a duplicate.
            "insert into rp62_u values (21, 9)",
            "insert into rp62_u values (24, 9) on conflict (b) where a % 3 = 0 do update set b = 1",
            "select a, b, n from rp62_u order by a",
            "drop table rp62_u",
        ],
    ),
    "a computed unique index another arbiter does not skip": (
        "RP-236",
        False,
        [
            "create table rp62_o (id int primary key, b int, a int)",
            "create unique index rp62_o_b on rp62_o (b) where a % 3 = 0",
            "insert into rp62_o values (1, 5, 3)",
            # Skipped by the primary key: never checked against rp62_o_b.
            "insert into rp62_o values (1, 5, 6) on conflict (id) do nothing",
            # Not skipped: rp62_o_b raises.
            "insert into rp62_o values (2, 5, 6) on conflict (id) do nothing",
            "insert into rp62_o values (2, 5, 7) on conflict (id) do nothing",
            "select id, b, a from rp62_o order by id",
            "drop table rp62_o",
        ],
    ),
    "DO UPDATE cannot affect a row a second time": (
        "RP-237",
        False,
        [
            "create table rp62_t (a int unique, n int default 0)",
            "insert into rp62_t values (1), (1) on conflict (a) do update set n = rp62_t.n + 1",
            "insert into rp62_t values (2), (2) on conflict (a) do nothing",
            "insert into rp62_t values (3)",
            "insert into rp62_t values (3), (3) on conflict (a) do update set n = rp62_t.n + 1",
            # Refused before the WHERE is evaluated.
            "insert into rp62_t values (4), (4) on conflict (a) do update set n = 1 where false",
            "create table rp62_c (a int, b int, n int default 0)",
            "create unique index rp62_c_b on rp62_c (b) where a % 3 = 0",
            "insert into rp62_c values (9, 7), (12, 7) on conflict (b) where a % 3 = 0"
            " do update set n = rp62_c.n + 10",
            "select a, n from rp62_t order by a",
            "select a, b, n from rp62_c order by a",
            "drop table rp62_t, rp62_c",
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
    "ON CONFLICT DO UPDATE over a computed unique index": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 2",
        [("6", "1", "1")],
        [("5", "1", "0")],
        "INSERT 0 2",
        "INSERT 0 1",
        ("ERR", "23505", 'duplicate key value violates unique constraint "rp62_u_b"', None),
        [
            ("4", "1", "0"),
            ("5", "1", "0"),
            ("6", "1", "1"),
            ("15", "None", "0"),
            ("18", "None", "0"),
            ("21", "9", "0"),
        ],
        "DROP TABLE",
    ],
    "a computed unique index another arbiter does not skip": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 1",
        "INSERT 0 0",
        ("ERR", "23505", 'duplicate key value violates unique constraint "rp62_o_b"', None),
        "INSERT 0 1",
        [("1", "5", "3"), ("2", "5", "7")],
        "DROP TABLE",
    ],
    "DO UPDATE cannot affect a row a second time": [
        "CREATE TABLE",
        (
            "ERR",
            "21000",
            "ON CONFLICT DO UPDATE command cannot affect row a second time",
            "Ensure that no rows proposed for "
            "insertion within the same command "
            "have duplicate constrained values.",
        ),
        "INSERT 0 1",
        "INSERT 0 1",
        (
            "ERR",
            "21000",
            "ON CONFLICT DO UPDATE command cannot affect row a second time",
            "Ensure that no rows proposed for "
            "insertion within the same command "
            "have duplicate constrained values.",
        ),
        (
            "ERR",
            "21000",
            "ON CONFLICT DO UPDATE command cannot affect row a second time",
            "Ensure that no rows proposed for "
            "insertion within the same command "
            "have duplicate constrained values.",
        ),
        "CREATE TABLE",
        "CREATE INDEX",
        (
            "ERR",
            "21000",
            "ON CONFLICT DO UPDATE command cannot affect row a second time",
            "Ensure that no rows proposed for "
            "insertion within the same command "
            "have duplicate constrained values.",
        ),
        [("2", "0"), ("3", "0")],
        [],
        "DROP TABLE",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
